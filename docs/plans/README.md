# Plans

Implementation plans for nlink-lab.

## Active Plans

_None._

## Completed

Plans 050–149 have been implemented and their plan files removed:

- 050–104: Core features, parser, CLI, containers, polish
- 105–119: DNS, macvlan/ipvlan, rich assertions, scenario DSL,
  CI integration, integration tests, benchmarks, Wi-Fi emulation,
  context-sensitive keywords, NAT, network subnet, shell-style run,
  route groups, link profiles, site grouping
- 120–127: IP computation functions, for-inside-blocks,
  site improvements, auto-addressing, conditional logic,
  auto-routing, fleet for_each imports, glob member patterns
- 128: Per-pair impairment matrix on shared networks
  (`impair A -- B { … }` inside `network { }`). Implementation lives
  on top of `nlink::netlink::impair::PerPeerImpairer` (shipped in
  nlink 0.15.1 in response to our spec); deploy step 14b builds one
  HTB+netem+flower tree per source interface.
- 129–149: NAT translate, editor/IDE support, mgmt bridge,
  spawn/wait-for/exec CLI, asymmetric impairments, healthcheck,
  partition/heal, IP discovery, CLI parameters, process capture,
  tcp-connect retry, network addresses, deploy suffix, validate
  show-ips, documentation gaps, external feedback triage +
  nlink 0.13.0 / 0.15.1 upgrades.

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLL_DSL_DESIGN.md](../NLL_DSL_DESIGN.md) | NLL language specification |
