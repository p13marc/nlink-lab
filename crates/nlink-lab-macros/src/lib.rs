//! Proc macros for nlink-lab integration testing.
//!
//! Provides `#[lab_test]` for writing integration tests that automatically
//! deploy a topology before the test and destroy it after.
//!
//! # Usage
//!
//! ```ignore
//! use nlink_lab::lab_test;
//!
//! // Deploy from a topology file
//! #[lab_test("examples/simple.toml")]
//! async fn test_ping(lab: RunningLab) {
//!     let out = lab.exec("host", "ping", &["-c1", "10.0.0.1"]).unwrap();
//!     assert_eq!(out.exit_code, 0);
//! }
//!
//! // Deploy from a builder function
//! #[lab_test(topology = my_topology)]
//! async fn test_custom(lab: RunningLab) {
//!     // ...
//! }
//!
//! fn my_topology() -> nlink_lab::Topology {
//!     nlink_lab::Lab::new("custom")
//!         .node("a", |n| n)
//!         .node("b", |n| n)
//!         .link("a:eth0", "b:eth0", |l| l.addresses("10.0.0.1/24", "10.0.0.2/24"))
//!         .build()
//! }
//! ```

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn, LitStr};

/// Attribute that wraps an async test with lab deploy/destroy lifecycle.
///
/// # Forms
///
/// - `#[lab_test("path/to/topology.toml")]` — deploy from file
/// - `#[lab_test("path/to/topology.nll")]` — deploy from NLL file
/// - `#[lab_test(topology = my_fn)]` — deploy from a function returning `Topology`
#[proc_macro_attribute]
pub fn lab_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);
    let fn_name = &input_fn.sig.ident;
    let fn_block = &input_fn.block;
    let fn_attrs = &input_fn.attrs;
    let fn_vis = &input_fn.vis;

    // Determine the topology source from the attribute
    let deploy_expr = if attr.is_empty() {
        // No attribute — error
        return syn::Error::new_spanned(
            &input_fn.sig,
            "lab_test requires a topology file path or `topology = fn_name`",
        )
        .to_compile_error()
        .into();
    } else {
        // Try to parse as a string literal first (file path)
        let attr2 = attr.clone();
        if let Ok(path) = syn::parse::<LitStr>(attr) {
            quote! {
                let __topo = nlink_lab::parser::parse_file(#path)
                    .expect("failed to parse topology file");
            }
        } else {
            // Try to parse as `topology = ident`
            let meta = parse_macro_input!(attr2 as LabTestArgs);
            let fn_ident = meta.topology_fn;
            quote! {
                let __topo = #fn_ident();
            }
        }
    };

    // Generate a unique lab name suffix from the test function name to avoid collisions
    let lab_name_suffix = fn_name.to_string();

    let expanded = quote! {
        #(#fn_attrs)*
        #[tokio::test]
        #fn_vis async fn #fn_name() {
            // Skip if not root
            if unsafe { libc::geteuid() } != 0 {
                eprintln!("skipping {}: requires root or CAP_NET_ADMIN", stringify!(#fn_name));
                return;
            }

            #deploy_expr

            // Override lab name with unique suffix to avoid parallel test collisions
            let mut __topo = __topo;
            let __original_name = __topo.lab.name.clone();
            __topo.lab.name = format!("{}-test-{}-{}", __original_name, #lab_name_suffix, std::process::id());

            let __result = __topo.validate();
            if __result.has_errors() {
                for e in __result.errors() {
                    eprintln!("  ERROR {e}");
                }
                panic!("topology validation failed");
            }

            let lab = __topo.deploy().await.expect("failed to deploy lab");

            // Use a guard for panic-safe cleanup
            struct __LabGuard {
                name: String,
            }
            impl Drop for __LabGuard {
                fn drop(&mut self) {
                    // Best-effort cleanup: delete namespaces matching prefix
                    let prefix = format!("{}-", self.name);
                    if let Ok(output) = std::process::Command::new("ip")
                        .args(["netns", "list"])
                        .output()
                    {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        for line in stdout.lines() {
                            let ns = line.split_whitespace().next().unwrap_or("");
                            if ns.starts_with(&prefix) {
                                let _ = std::process::Command::new("ip")
                                    .args(["netns", "delete", ns])
                                    .status();
                            }
                        }
                    }
                    let _ = nlink_lab::state::remove(&self.name);
                }
            }

            let __guard = __LabGuard { name: lab.name().to_string() };

            // Run the test body
            {
                #fn_block
            }

            // Clean destroy (guard handles panics)
            std::mem::forget(__guard);
            lab.destroy().await.expect("failed to destroy lab");
        }
    };

    expanded.into()
}

/// Parse `topology = my_fn` from attribute args.
struct LabTestArgs {
    topology_fn: syn::Ident,
}

impl syn::parse::Parse for LabTestArgs {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let ident: syn::Ident = input.parse()?;
        if ident != "topology" {
            return Err(syn::Error::new_spanned(
                ident,
                "expected `topology = fn_name`",
            ));
        }
        let _: syn::Token![=] = input.parse()?;
        let topology_fn: syn::Ident = input.parse()?;
        Ok(Self { topology_fn })
    }
}
