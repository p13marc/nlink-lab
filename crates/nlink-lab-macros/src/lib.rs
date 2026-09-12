//! Proc macros for nlink-lab integration testing.
//!
//! Provides `#[lab_test]` for writing integration tests that automatically
//! deploy a topology before the test and destroy it after.
//!
//! The expansion only refers to items by absolute path through
//! `::nlink_lab` (tokio is reached via
//! `::nlink_lab::test_helpers::__macro_support::tokio`), so a consumer
//! crate needs nothing beyond `nlink-lab` in its `[dev-dependencies]`.
//!
//! # Privileges
//!
//! Deploying a lab needs root (or `CAP_NET_ADMIN`). A test that runs
//! without it **fails** with a clear message; set
//! `NLINK_LAB_SKIP_ROOT_TESTS=1` to turn that into a loud skip instead.
//!
//! # Usage
//!
//! ```ignore
//! use nlink_lab::lab_test;
//!
//! // Deploy from a topology file
//! #[lab_test("examples/simple.nll")]
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
///
/// // With capture-on-failure: every node:iface gets a live pcap
/// // for the test duration. On panic, pcaps are persisted to
/// // target/lab_test_captures/<test>-<pid>/. On success, discarded.
/// #[lab_test("simple.nll", capture = true)]
/// async fn test_with_pcaps(lab: RunningLab) { ... }
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
                    let __topo = ::nlink_lab::parser::parse_file(#abs_path)
                        .expect("failed to parse topology file");
                }
            } else {
                let pairs = args
                    .set
                    .iter()
                    .map(|(k, v)| quote! { (#k.into(), #v.into()) });
                quote! {
                    let __params: ::std::vec::Vec<(::std::string::String, ::std::string::String)> =
                        ::std::vec![ #(#pairs),* ];
                    let __topo = ::nlink_lab::parser::parse_file_with_params(
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

    // Optional capture-on-failure wiring.
    let capture_setup = if args.capture_on_failure {
        quote! {
            // Spin up parallel pcaps, one per (namespace, iface).
            // The guard's Drop checks std::thread::panicking() —
            // if true, pcaps are persisted to a discoverable
            // path; otherwise they're wiped with the temp dir.
            let __cap_targets = lab.capture_targets();
            let __lab_capture = ::nlink_lab::test_helpers::LabCapture::start(&__cap_targets)
                .map_err(|e| ::std::eprintln!("lab_capture: failed to start: {e}"))
                .ok();

            struct __CaptureGuard {
                cap: Option<::nlink_lab::test_helpers::LabCapture>,
                dest: ::std::path::PathBuf,
            }
            impl Drop for __CaptureGuard {
                fn drop(&mut self) {
                    if let Some(cap) = self.cap.take() {
                        let failed = ::std::thread::panicking();
                        match cap.persist_on_failure_in(failed, &self.dest) {
                            ::std::result::Result::Ok(::std::option::Option::Some(paths)) => {
                                ::std::eprintln!(
                                    "lab_capture: persisted {} pcap(s) to {}",
                                    paths.len(),
                                    self.dest.display(),
                                );
                                for p in paths {
                                    ::std::eprintln!("  - {}", p.display());
                                }
                            }
                            // success path — discarded
                            ::std::result::Result::Ok(::std::option::Option::None) => {}
                            ::std::result::Result::Err(e) => {
                                ::std::eprintln!("lab_capture: persist failed: {e}")
                            }
                        }
                    }
                }
            }
            // Persist directory under target/lab_test_captures/<test-name>-<pid>/
            let __cap_dest = ::std::path::PathBuf::from(
                ::std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into()),
            )
            .join("lab_test_captures")
            .join(::std::format!("{}-{}", #lab_name_suffix, ::std::process::id()));

            let __cap_guard = __CaptureGuard {
                cap: __lab_capture,
                dest: __cap_dest,
            };
        }
    } else {
        quote! {}
    };

    // Optional timeout wrapping the test body.
    let timeout_secs = args.timeout_secs;
    let body_with_timeout = if let Some(secs) = timeout_secs {
        quote! {
            // Non-`move` block: `lab` is borrowed by the body and is
            // still available for the `destroy()` that follows.
            if ::nlink_lab::test_helpers::__macro_support::tokio::time::timeout(
                ::std::time::Duration::from_secs(#secs),
                async { #fn_block }
            ).await.is_err() {
                ::std::panic!(
                    "lab_test '{}' exceeded {}s timeout",
                    ::std::stringify!(#fn_name),
                    #secs,
                );
            }
        }
    } else {
        quote! { #fn_block }
    };

    // The generated test is a plain `#[test]` that builds its own
    // current-thread tokio runtime (what `#[tokio::test]` would do)
    // through nlink-lab's re-export, so consumers don't need `tokio`
    // as a dev-dependency just for the expansion to compile.
    let expanded = quote! {
        #(#fn_attrs)*
        #[::core::prelude::v1::test]
        #fn_vis fn #fn_name() {
            // Privilege check. Default: FAIL when not root, so a
            // non-root `cargo test` can't report privileged tests as
            // green with zero coverage. NLINK_LAB_SKIP_ROOT_TESTS=1
            // turns this into a loud skip.
            match ::nlink_lab::test_helpers::root_gate(::std::stringify!(#fn_name)) {
                ::nlink_lab::test_helpers::RootGate::Proceed => {}
                ::nlink_lab::test_helpers::RootGate::Skip => return,
                ::nlink_lab::test_helpers::RootGate::Fail(msg) => ::std::panic!("{}", msg),
            }

            let __rt = ::nlink_lab::test_helpers::__macro_support::tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime for #[lab_test]");

            __rt.block_on(async {
                #deploy_expr

                // Override lab name with unique suffix to avoid parallel test collisions
                let mut __topo = __topo;
                let __original_name = __topo.lab.name.clone();
                __topo.lab.name = ::std::format!(
                    "{}-test-{}-{}",
                    __original_name,
                    #lab_name_suffix,
                    ::std::process::id(),
                );

                let __result = __topo.validate();
                if __result.has_errors() {
                    for e in __result.errors() {
                        ::std::eprintln!("  ERROR {e}");
                    }
                    ::std::panic!("topology validation failed");
                }

                // Panic-safe cleanup guard. Armed *before* deploy so a
                // deploy that fails half-way is swept too. On drop
                // (panic anywhere below, or a failed destroy) it runs
                // `test_helpers::cleanup_lab_blocking`, which prefers
                // `RunningLab::load(..).destroy()` and falls back to
                // a state-less sweep (namespaces via nlink, mgmt
                // links, containers, hwsim, /etc/hosts, subnet pool,
                // state dir). Disarmed only after a successful destroy.
                struct __LabGuard {
                    topo: ::std::option::Option<::nlink_lab::Topology>,
                }
                impl __LabGuard {
                    fn disarm(&mut self) {
                        self.topo = ::std::option::Option::None;
                    }
                }
                impl ::std::ops::Drop for __LabGuard {
                    fn drop(&mut self) {
                        if let ::std::option::Option::Some(topo) = self.topo.take() {
                            let report = ::nlink_lab::test_helpers::cleanup_lab_blocking(&topo);
                            for w in &report.warnings {
                                ::std::eprintln!(
                                    "lab_test '{}': cleanup warning: {w}",
                                    ::std::stringify!(#fn_name),
                                );
                            }
                        }
                    }
                }
                let mut __guard = __LabGuard {
                    topo: ::std::option::Option::Some(__topo.clone()),
                };

                // `mut` so test bodies can call `&mut self` methods like
                // `spawn_with_logs` without having to shadow the binding.
                let mut lab = __topo.deploy().await.expect("failed to deploy lab");

                // Optional capture-on-failure setup (no-op when not enabled).
                #capture_setup

                // Run the test body (optionally wrapped in a timeout).
                #body_with_timeout

                // Clean destroy. If it fails, the panic unwinds through
                // the still-armed guard, which sweeps whatever is left.
                lab.destroy().await.expect("failed to destroy lab");
                __guard.disarm();
            });
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
    capture_on_failure: bool,
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
        let mut capture_on_failure = false;

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
            } else if kw == "capture" {
                let _: syn::Token![=] = input.parse()?;
                // Accept `capture = true` (the only form today).
                let lit: syn::LitBool = input.parse()?;
                capture_on_failure = lit.value;
            } else {
                return Err(syn::Error::new_spanned(
                    kw,
                    "unknown lab_test arg — expected `set { … }`, `timeout = SECS`, or `capture = true`",
                ));
            }
        }

        Ok(Self {
            source,
            set,
            timeout_secs,
            capture_on_failure,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{LabTestArgs, LabTestSource};

    fn parse(src: &str) -> syn::Result<LabTestArgs> {
        syn::parse_str::<LabTestArgs>(src)
    }

    #[test]
    fn path_form() {
        let args = parse(r#""examples/simple.nll""#).unwrap();
        assert!(
            matches!(args.source, LabTestSource::Path(ref p) if p.value() == "examples/simple.nll")
        );
        assert!(args.set.is_empty());
        assert_eq!(args.timeout_secs, None);
        assert!(!args.capture_on_failure);
    }

    #[test]
    fn topology_fn_form() {
        let args = parse("topology = my_topology").unwrap();
        assert!(matches!(args.source, LabTestSource::Function(ref f) if f == "my_topology"));
    }

    #[test]
    fn all_modifiers() {
        let args = parse(
            r#""wan.nll", set { delay = "20ms", loss = "0.5%" }, timeout = 30, capture = true,"#,
        )
        .unwrap();
        assert_eq!(
            args.set,
            vec![
                ("delay".to_string(), "20ms".to_string()),
                ("loss".to_string(), "0.5%".to_string()),
            ]
        );
        assert_eq!(args.timeout_secs, Some(30));
        assert!(args.capture_on_failure);
    }

    #[test]
    fn rejects_unknown_modifier() {
        let err = parse(r#""a.nll", retries = 3"#).err().expect("should fail");
        assert!(err.to_string().contains("unknown lab_test arg"), "{err}");
    }

    #[test]
    fn rejects_bad_source() {
        let err = parse("topo = f").err().expect("should fail");
        assert!(err.to_string().contains("expected a path literal"), "{err}");
    }
}
