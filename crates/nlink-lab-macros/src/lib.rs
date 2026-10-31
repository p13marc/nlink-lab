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
//! Deploying a lab needs root (or `CAP_NET_ADMIN` + `CAP_SYS_ADMIN`). A
//! test that runs without it **fails** with a clear message; set
//! `NLINK_LAB_SKIP_ROOT_TESTS=1` to turn that into a loud skip instead. A
//! test written without the macro uses the same gate,
//! `nlink_lab::test_helpers::privileged`.
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
///
/// // With a fixed seed: every netem draws the same losses on every run
/// // (#337). Without one, a seed is drawn and printed when the test
/// // fails, with the command that replays it (`NLINK_LAB_SEED=N`).
/// #[lab_test("lossy.nll", seed = 42)]
/// async fn test_lossy(lab: RunningLab) { ... }
///
/// // A failing test leaves a bundle of everything needed to debug it in
/// // target/nlink-lab/<test>/ (or $NLINK_LAB_BUNDLE_DIR/<lab>/): logs,
/// // status, events, the nodes' addresses, routes, rules, nftables, qdiscs
/// // and conntrack, and the pcaps of `capture = true` (#338). Off with:
/// #[lab_test("simple.nll", bundle = false)]
/// async fn test_without_a_bundle(lab: RunningLab) { ... }
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
                    // a key no `param` declares was a warning nobody saw (#222)
                    let __declared = ::nlink_lab::parser::declared_params(#abs_path)
                        .expect("failed to parse topology file");
                    for (key, _) in &__params {
                        if !__declared.contains(key) {
                            ::std::panic!(
                                "lab_test: `set {{ {key} = … }}`: {} declares no `param {key}` (it declares: {})",
                                #abs_path,
                                __declared.join(", "),
                            );
                        }
                    }
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
    let ws_root = workspace_root();
    // The failure bundle (#338): on by default, `bundle = false` off. It
    // takes the pcaps a `capture = true` test kept.
    let bundle_on = args.bundle;
    let bundle_pcaps = if args.capture_on_failure {
        quote! {
            ::std::option::Option::Some(::nlink_lab::test_helpers::capture_dir(#ws_root, #lab_name_suffix))
        }
    } else {
        quote! { ::std::option::Option::None }
    };
    let bundle_main = quote! {
        #[allow(unused_variables)]
        let __bundle = ::nlink_lab::test_helpers::__macro_support::BundleSpec::new(
            #bundle_on,
            #ws_root,
            ::std::env!("CARGO_PKG_NAME"),
            ::std::module_path!(),
            ::std::stringify!(#fn_name),
            #bundle_pcaps,
        );
        let __bundle_worker = __bundle.clone();
    };
    let seed_attr = match args.seed {
        Some(s) => quote! { ::std::option::Option::Some(#s) },
        None => quote! { ::std::option::Option::None },
    };

    // Optional capture-on-failure wiring: a slot both threads can reach,
    // so a timeout keeps the pcaps too.
    let (capture_main, capture_setup, capture_on_timeout) = if args.capture_on_failure {
        (
            quote! {
                let __cap = ::nlink_lab::test_helpers::CaptureSlot::new(
                    ::nlink_lab::test_helpers::capture_dir(#ws_root, #lab_name_suffix),
                );
                let __cap_worker = __cap.clone();
            },
            // One pcap per (namespace, iface). The guard keeps them when
            // the body panics and wipes them otherwise.
            quote! {
                __cap_worker.start(&lab);
                let __cap_guard = __cap_worker.guard();
            },
            quote! { __cap.finish(true); },
        )
    } else {
        (quote! {}, quote! {}, quote! {})
    };

    // The timeout form tells the watching thread what to sweep.
    let announce = if args.timeout_secs.is_some() {
        quote! { let _ = __armed.send(__topo.clone()); }
    } else {
        quote! {}
    };

    let lab_body = quote! {
        #deploy_expr

        // A unique suffix on the name, and on the prefix when the
        // topology sets one: namespaces are named after the prefix, so
        // two tests of a prefixed topology collided (#222).
        let mut __topo = __topo;
        __topo.lab.add_suffix(&::std::format!(
            "test-{}-{}",
            #lab_name_suffix,
            ::std::process::id(),
        ));

        // The seed (#337): NLINK_LAB_SEED (a replay), else `seed = N`,
        // else the topology's, else one drawn now, so a failure can say
        // which. A kernel without netem seeds gets none at deploy.
        if let ::std::option::Option::Some(seed) =
            ::nlink_lab::seed::from_env().expect("lab_test: NLINK_LAB_SEED")
        {
            __topo.lab.seed = ::std::option::Option::Some(seed);
        } else if let ::std::option::Option::Some(seed) = #seed_attr {
            __topo.lab.seed = ::std::option::Option::Some(seed);
        }
        if __topo.lab.seed.is_none() {
            __topo.lab.seed = ::std::option::Option::Some(::nlink_lab::seed::draw());
        }
        let _seed_note = ::nlink_lab::test_helpers::__macro_support::SeedNote::new(
            __topo.lab.seed,
            ::std::env!("CARGO_PKG_NAME"),
            ::std::module_path!(),
            ::std::stringify!(#fn_name),
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
        #announce

        // `mut` so test bodies can call `&mut self` methods like
        // `spawn_with_logs` without having to shadow the binding.
        let mut lab = __topo.deploy().await.expect("failed to deploy lab");
        // Dropped after the capture guard (declared later), so a failing
        // test's pcaps are kept first, and before the lab guard sweeps the
        // lab: the bundle is collected while the lab still exists (#338).
        let _bundle_guard = __bundle_worker.guard(lab.name(), lab.topology().lab.seed);

        #capture_setup

        #fn_block

        // Clean destroy. If it fails, the panic unwinds through
        // the still-armed guard, which sweeps whatever is left.
        lab.destroy().await.expect("failed to destroy lab");
        __guard.disarm();
    };

    let runtime = quote! {
        ::nlink_lab::test_helpers::__macro_support::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime for #[lab_test]")
    };

    // With a timeout the test runs on its own thread, watched from this
    // one: `tokio::time::timeout` could not fire while the body blocked
    // its thread in a synchronous call such as `lab.exec` (#222). At the
    // deadline the pcaps are kept, the lab is swept and the test fails;
    // the stuck thread is left behind.
    let runner = if let Some(secs) = args.timeout_secs {
        quote! {
            let (__armed, __armed_rx) = ::std::sync::mpsc::channel::<::nlink_lab::Topology>();
            let __worker = ::std::thread::Builder::new()
                .name(::std::string::String::from(::std::stringify!(#fn_name)))
                .spawn(move || {
                    let __rt = #runtime;
                    __rt.block_on(async { #lab_body });
                })
                .expect("failed to start the #[lab_test] thread");
            let __deadline =
                ::std::time::Instant::now() + ::std::time::Duration::from_secs(#secs);
            while !__worker.is_finished() {
                if ::std::time::Instant::now() >= __deadline {
                    #capture_on_timeout
                    if let ::std::result::Result::Ok(topo) = __armed_rx.try_recv() {
                        __bundle.collect(
                            &topo.lab.name,
                            topo.lab.seed,
                            &::std::format!("it exceeded its {}s timeout", #secs),
                        );
                        ::nlink_lab::test_helpers::__macro_support::SeedNote::new(
                            topo.lab.seed,
                            ::std::env!("CARGO_PKG_NAME"),
                            ::std::module_path!(),
                            ::std::stringify!(#fn_name),
                        )
                        .print();
                        let report = ::nlink_lab::test_helpers::cleanup_lab_blocking(&topo);
                        for w in &report.warnings {
                            ::std::eprintln!(
                                "lab_test '{}': cleanup warning: {w}",
                                ::std::stringify!(#fn_name),
                            );
                        }
                    }
                    ::std::panic!(
                        "lab_test '{}' exceeded {}s timeout",
                        ::std::stringify!(#fn_name),
                        #secs,
                    );
                }
                ::std::thread::sleep(::std::time::Duration::from_millis(20));
            }
            if let ::std::result::Result::Err(payload) = __worker.join() {
                ::std::panic::resume_unwind(payload);
            }
        }
    } else {
        quote! {
            let __rt = #runtime;
            __rt.block_on(async { #lab_body });
        }
    };

    // The generated test is a plain `#[test]` that builds its own
    // current-thread tokio runtime (what `#[tokio::test]` would do)
    // through nlink-lab's re-export, so consumers don't need `tokio`
    // as a dev-dependency just for the expansion to compile.
    let expanded = quote! {
        #(#fn_attrs)*
        #[::core::prelude::v1::test]
        #fn_vis fn #fn_name() {
            // Privilege check. Default: FAIL without root or the
            // capabilities, so an unprivileged `cargo test` can't report
            // privileged tests as green with zero coverage.
            // NLINK_LAB_SKIP_ROOT_TESTS=1 turns this into a loud skip.
            match ::nlink_lab::test_helpers::root_gate(::std::stringify!(#fn_name)) {
                ::nlink_lab::test_helpers::RootGate::Proceed => {}
                ::nlink_lab::test_helpers::RootGate::Skip => return,
                ::nlink_lab::test_helpers::RootGate::Fail(msg) => ::std::panic!("{}", msg),
            }

            #capture_main
            #bundle_main

            #runner
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
///              | "capture" "=" LitBool
///              | "seed" "=" LitInt             (#337)
///              | "bundle" "=" LitBool          (#338; default true)
/// KeyValue    := (Ident | LitStr) "=" LitStr
/// ```
struct LabTestArgs {
    source: LabTestSource,
    set: Vec<(String, String)>,
    timeout_secs: Option<u64>,
    capture_on_failure: bool,
    seed: Option<u64>,
    bundle: bool,
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
        let mut seed: Option<u64> = None;
        let mut bundle = true;

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
                    // `type = …` (a keyword) and `"wan-delay" = …` (not an
                    // identifier) name NLL params too (#222)
                    let key = if content.peek(LitStr) {
                        content.parse::<LitStr>()?.value()
                    } else {
                        syn::ext::IdentExt::parse_any(&content)
                            .map(|i: syn::Ident| i.to_string())?
                    };
                    let _: syn::Token![=] = content.parse()?;
                    let val: LitStr = content.parse()?;
                    set.push((key, val.value()));
                    if content.is_empty() {
                        break;
                    }
                    let _: syn::Token![,] = content.parse()?;
                }
            } else if kw == "timeout" {
                let _: syn::Token![=] = input.parse()?;
                let lit: LitInt = input.parse()?;
                timeout_secs = Some(lit.base10_parse()?);
            } else if kw == "seed" {
                let _: syn::Token![=] = input.parse()?;
                let lit: LitInt = input.parse()?;
                let n: u64 = lit.base10_parse()?;
                if n > i64::MAX as u64 {
                    return Err(syn::Error::new_spanned(
                        lit,
                        "a seed is at most 9223372036854775807",
                    ));
                }
                seed = Some(n);
            } else if kw == "bundle" {
                let _: syn::Token![=] = input.parse()?;
                let lit: syn::LitBool = input.parse()?;
                bundle = lit.value;
            } else if kw == "capture" {
                let _: syn::Token![=] = input.parse()?;
                // Accept `capture = true` (the only form today).
                let lit: syn::LitBool = input.parse()?;
                capture_on_failure = lit.value;
            } else {
                return Err(syn::Error::new_spanned(
                    kw,
                    "unknown lab_test arg — expected `set { … }`, `timeout = SECS`, `capture = true`, `seed = N`, or `bundle = false`",
                ));
            }
        }

        Ok(Self {
            source,
            set,
            timeout_secs,
            capture_on_failure,
            seed,
            bundle,
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
            r#""wan.nll", set { delay = "20ms", loss = "0.5%" }, timeout = 30, capture = true, seed = 42,"#,
        )
        .unwrap();
        assert_eq!(args.seed, Some(42));
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
    fn set_keys_can_be_keywords_or_strings() {
        let args = parse(r#""p.nll", set { type = "a", "wan-delay" = "10ms" }"#).unwrap();
        assert_eq!(
            args.set,
            vec![
                ("type".to_string(), "a".to_string()),
                ("wan-delay".to_string(), "10ms".to_string()),
            ]
        );
    }

    #[test]
    fn a_bundle_is_on_unless_turned_off() {
        assert!(parse(r#""a.nll""#).unwrap().bundle);
        assert!(!parse(r#""a.nll", bundle = false"#).unwrap().bundle);
    }

    #[test]
    fn a_seed_fits_the_state() {
        assert_eq!(parse(r#""a.nll""#).unwrap().seed, None);
        assert_eq!(
            parse(r#""a.nll", seed = 9223372036854775807"#)
                .unwrap()
                .seed,
            Some(i64::MAX as u64)
        );
        assert!(parse(r#""a.nll", seed = 9223372036854775808"#).is_err());
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
