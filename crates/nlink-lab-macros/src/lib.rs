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
use syn::{ItemFn, LitInt, LitStr, braced, parse_macro_input};

/// Attribute that wraps an async test with lab deploy/destroy lifecycle.
///
/// # Forms
///
/// ```ignore
/// // Path to an NLL file
/// #[lab_test("examples/simple.nll")]
/// async fn test_basic(lab: RunningLab) { ... }
///
/// // Builder-function form
/// #[lab_test(topology = my_fn)]
/// async fn test_custom(lab: RunningLab) { ... }
///
/// // With NLL `param` overrides (mirrors CLI `--set k=v`)
/// #[lab_test("wan.nll", set { delay = "20ms", loss = "0.5%" })]
/// async fn test_wan(lab: RunningLab) { ... }
///
/// // With a per-test timeout (test panics if it exceeds N seconds)
/// #[lab_test("simple.nll", timeout = 30)]
/// async fn test_must_finish_fast(lab: RunningLab) { ... }
/// ```
///
/// `set { ... }` keys are NLL `param` names; values are string-typed
/// (the param's declared type does the cast at lower time).
#[proc_macro_attribute]
pub fn lab_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);
    let fn_name = &input_fn.sig.ident;
    let fn_block = &input_fn.block;
    let fn_attrs = &input_fn.attrs;
    let fn_vis = &input_fn.vis;

    if attr.is_empty() {
        return syn::Error::new_spanned(
            &input_fn.sig,
            "lab_test requires a topology file path or `topology = fn_name`",
        )
        .to_compile_error()
        .into();
    }

    let args = parse_macro_input!(attr as LabTestArgs);

    // Resolve relative paths against the workspace root at compile time
    // so tests work regardless of the runtime working directory.
    let workspace_root = || -> String {
        std::env::var("CARGO_WORKSPACE_DIR").unwrap_or_else(|_| {
            let manifest_dir =
                std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
            let mut dir = std::path::PathBuf::from(&manifest_dir);
            loop {
                let cargo_toml = dir.join("Cargo.toml");
                if cargo_toml.exists()
                    && let Ok(contents) = std::fs::read_to_string(&cargo_toml)
                    && contents.contains("[workspace]")
                {
                    return dir.to_string_lossy().to_string();
                }
                if !dir.pop() {
                    return manifest_dir;
                }
            }
        })
    };

    let deploy_expr = match &args.source {
        LabTestSource::Path(path) => {
            let abs_path = std::path::Path::new(&workspace_root())
                .join(path.value())
                .to_string_lossy()
                .to_string();
            if args.set.is_empty() {
                quote! {
                    let __topo = nlink_lab::parser::parse_file(#abs_path)
                        .expect("failed to parse topology file");
                }
            } else {
                let pairs = args
                    .set
                    .iter()
                    .map(|(k, v)| quote! { (#k.into(), #v.into()) });
                quote! {
                    let __params: Vec<(String, String)> = vec![ #(#pairs),* ];
                    let __topo = nlink_lab::parser::parse_file_with_params(
                        #abs_path,
                        &__params,
                    ).expect("failed to parse topology file with params");
                }
            }
        }
        LabTestSource::Function(fn_ident) => {
            if !args.set.is_empty() {
                return syn::Error::new_spanned(
                    fn_ident,
                    "`set { … }` overrides only apply to file-path topologies; \
                     a `topology = fn` form should configure params inside the function",
                )
                .to_compile_error()
                .into();
            }
            quote! {
                let __topo = #fn_ident();
            }
        }
    };

    let lab_name_suffix = fn_name.to_string();

    // Optional timeout wrapping the test body.
    let timeout_secs = args.timeout_secs;
    let body_with_timeout = if let Some(secs) = timeout_secs {
        quote! {
            if let Err(_) = tokio::time::timeout(
                std::time::Duration::from_secs(#secs),
                async move { #fn_block }
            ).await {
                panic!(
                    "lab_test '{}' exceeded {}s timeout",
                    stringify!(#fn_name),
                    #secs,
                );
            }
        }
    } else {
        quote! { #fn_block }
    };

    let expanded = quote! {
        #(#fn_attrs)*
        #[tokio::test]
        #fn_vis async fn #fn_name() {
            // Skip if not root. The skip is loud so it doesn't look
            // like a passing test in CI logs — non-root runs of
            // privileged tests are a common foot-gun.
            if unsafe { libc::geteuid() } != 0 {
                eprintln!(
                    "\n*** SKIPPING #[lab_test] '{}' — requires root or CAP_NET_ADMIN ***\n\
                     ***   Run with `sudo cargo test` or grant CAP_NET_ADMIN to cargo. ***",
                    stringify!(#fn_name),
                );
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

            // Run the test body (optionally wrapped in a timeout).
            #body_with_timeout

            // Clean destroy (guard handles panics)
            std::mem::forget(__guard);
            lab.destroy().await.expect("failed to destroy lab");
        }
    };

    expanded.into()
}

/// Parsed attribute args for `#[lab_test(...)]`.
///
/// Grammar (informal):
///
/// ```text
/// LabTestArgs := Source ( "," Modifier )*
/// Source      := LitStr  |  "topology" "=" Ident
/// Modifier    := "set" "{" KeyValue ( "," KeyValue )* "}"
///              | "timeout" "=" LitInt          (seconds)
/// KeyValue    := Ident "=" LitStr
/// ```
struct LabTestArgs {
    source: LabTestSource,
    set: Vec<(String, String)>,
    timeout_secs: Option<u64>,
}

enum LabTestSource {
    Path(LitStr),
    Function(syn::Ident),
}

impl syn::parse::Parse for LabTestArgs {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        // Source first (positional).
        let source = if input.peek(LitStr) {
            LabTestSource::Path(input.parse()?)
        } else {
            let ident: syn::Ident = input.parse()?;
            if ident != "topology" {
                return Err(syn::Error::new_spanned(
                    ident,
                    "expected a path literal or `topology = fn_name`",
                ));
            }
            let _: syn::Token![=] = input.parse()?;
            LabTestSource::Function(input.parse()?)
        };

        // Optional modifiers.
        let mut set: Vec<(String, String)> = Vec::new();
        let mut timeout_secs: Option<u64> = None;

        while !input.is_empty() {
            let _: syn::Token![,] = input.parse()?;
            if input.is_empty() {
                break; // trailing comma
            }
            let kw: syn::Ident = input.parse()?;
            if kw == "set" {
                let content;
                braced!(content in input);
                while !content.is_empty() {
                    let key: syn::Ident = content.parse()?;
                    let _: syn::Token![=] = content.parse()?;
                    let val: LitStr = content.parse()?;
                    set.push((key.to_string(), val.value()));
                    if content.is_empty() {
                        break;
                    }
                    let _: syn::Token![,] = content.parse()?;
                }
            } else if kw == "timeout" {
                let _: syn::Token![=] = input.parse()?;
                let lit: LitInt = input.parse()?;
                timeout_secs = Some(lit.base10_parse()?);
            } else {
                return Err(syn::Error::new_spanned(
                    kw,
                    "unknown lab_test arg — expected `set { … }` or `timeout = SECS`",
                ));
            }
        }

        Ok(Self {
            source,
            set,
            timeout_secs,
        })
    }
}
