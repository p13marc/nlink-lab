; NLL syntax highlighting queries for tree-sitter

; ── Structural keywords ──────────────────────────
["lab" "node" "link" "network" "profile" "for" "in" "let" "import"
 "as" "if" "site" "param" "defaults" "pool" "validate" "scenario"
 "benchmark" "mesh" "ring" "star" "for_each"] @keyword

; ── Context-sensitive keywords ───────────────────
(forward_property "forward" @keyword)
(forward_property ["ipv4" "ipv6"] @keyword)
(route_property "route" @keyword)
(route_params ["via" "dev" "metric"] @keyword)
(route_destination "default" @keyword)
(firewall_block "firewall" @keyword)
(firewall_block "policy" @keyword)
(firewall_rule ["accept" "drop"] @keyword)
(nat_block "nat" @keyword)
(nat_rule ["masquerade" "dnat" "snat" "translate"] @keyword)
(nat_rule ["src" "dst" "to"] @keyword)
(impairment_properties ["delay" "jitter" "loss" "corrupt" "reorder" "rate"] @keyword)
(directional_impairment ["->" "<-"] @keyword.operator)
(rate_properties ["egress" "ingress"] @keyword)
(sysctl_property "sysctl" @keyword)
(loopback_property "lo" @keyword)
(image_property "image" @keyword)
(image_property "cmd" @keyword)
(run_property "run" @keyword)
(run_property "background" @keyword)
(lab_property ["description" "prefix" "runtime" "version" "author" "tags" "mgmt" "dns" "routing"] @keyword)
(lab_property ["hosts" "off" "auto" "manual"] @keyword)
(vrf_block "vrf" @keyword)
(vrf_block "table" @keyword)
(wireguard_block "wireguard" @keyword)
(vxlan_block "vxlan" @keyword)
(macvlan_block "macvlan" @keyword)
(ipvlan_block "ipvlan" @keyword)
(wifi_block "wifi" @keyword)
(dummy_block "dummy" @keyword)
(match_expression ["src" "dst" "tcp" "udp" "dport" "sport" "icmp" "ct" "mark" "type" "state"] @keyword)
(scenario_action ["down" "up" "clear" "exec" "log"] @keyword)
(scenario_step "at" @keyword)
(benchmark_test ["iperf3" "ping"] @keyword)
(benchmark_property ["duration" "streams" "udp" "count" "assert" "above" "below"] @keyword)
(container_property ["cpu" "memory" "privileged" "cap-add" "cap-drop" "entrypoint" "hostname" "workdir" "labels" "pull" "exec" "healthcheck" "startup-delay" "env-file" "config" "overlay" "depends-on" "env" "volumes"] @keyword)

; ── Operators ────────────────────────────────────
["--" "=" "==" "!=" ".." "<" ">" "<=" ">=" "&&" "||"] @operator
"/" @operator

; ── Punctuation ──────────────────────────────────
["{" "}"] @punctuation.bracket
["[" "]"] @punctuation.bracket
["(" ")"] @punctuation.bracket
":" @punctuation.delimiter
"," @punctuation.delimiter

; ── Strings ──────────────────────────────────────
(string) @string

; ── Network literals ─────────────────────────────
(cidr) @number
(ipv4_address) @number
(ipv6_cidr) @number
(ipv6_address) @number

; ── Numeric literals ─────────────────────────────
(integer) @number
(duration) @number
(rate) @number
(percent) @number

; ── Interpolation ────────────────────────────────
(interpolation) @string.special

; ── Names and definitions ────────────────────────
(node_definition
  (identifier) @type)
(profile_definition
  (identifier) @type.definition)
(network_definition
  (identifier) @type)
(site_block
  (identifier) @type)
(pool_definition
  (identifier) @type)
(defaults_definition
  (identifier) @type)
(pattern_definition
  (identifier) @type)

; ── Variables ────────────────────────────────────
(let_binding
  (identifier) @variable)
(for_loop
  (identifier) @variable)
(param_definition
  (identifier) @variable)

; ── Function calls ───────────────────────────────
(function_call
  (identifier) @function)

; ── Endpoints ────────────────────────────────────
(endpoint
  (identifier) @variable
  (identifier) @property)

; ── Import ───────────────────────────────────────
(import_statement
  (identifier) @type)

; ── Comments ─────────────────────────────────────
(line_comment) @comment
(block_comment) @comment
