/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

// NLL (nlink-lab Language) grammar for tree-sitter.
//
// NLL is a topology definition DSL with context-sensitive keywords,
// interpolation, typed literals (CIDR, duration, rate, percent),
// and block-based structure.
//
// Conformance: `tree-sitter parse` must succeed on every file under
// `examples/`, and `crates/nlink-lab/tests/editor_keywords.rs` checks
// that every keyword the Rust lexer/parser knows appears here (and that
// nothing here claims a keyword the language does not have).

module.exports = grammar({
  name: "nll",

  extras: ($) => [/\s/, $.line_comment, $.block_comment],

  // Keyword extraction: a string literal that also matches `identifier`
  // is a keyword only where the grammar expects it, so `node mode` and
  // `wifi wlan0 mode ap` both parse.
  word: ($) => $.identifier,

  conflicts: ($) => [[$._value, $._name]],

  rules: {
    source_file: ($) => repeat($._statement),

    // ── Top-level statements ────────────────────────
    _statement: ($) =>
      choice(
        $.import_statement,
        $.lab_declaration,
        $.profile_definition,
        $.node_definition,
        $.link_definition,
        $.network_definition,
        $.impairment_statement,
        $.rate_statement,
        $.defaults_definition,
        $.pool_definition,
        $.pattern_definition,
        $.validate_block,
        $.scenario_block,
        $.benchmark_block,
        $.let_binding,
        $.for_loop,
        $.if_block,
        $.site_block,
        $.param_definition,
      ),

    // ── Import ──────────────────────────────────────
    import_statement: ($) =>
      choice(
        seq(
          "import",
          $.string,
          "as",
          $.identifier,
          optional(seq("(", $.param_list, ")")),
        ),
        // fleet import: one instance per alias
        seq("import", $.string, "for_each", "{", repeat($.import_item), "}"),
      ),

    import_item: ($) =>
      seq($.identifier, optional(seq("(", $.param_list, ")"))),

    param_list: ($) =>
      seq($.param_assign, repeat(seq(",", $.param_assign))),

    param_assign: ($) => seq($.identifier, "=", $._value),

    // ── Lab declaration ─────────────────────────────
    lab_declaration: ($) =>
      seq("lab", $.string, optional(seq("{", repeat($.lab_property), "}"))),

    lab_property: ($) =>
      choice(
        seq("description", $.string),
        seq("prefix", $.string),
        seq("runtime", $.string),
        seq("version", $.string),
        seq("author", $.string),
        seq("tags", $.list),
        seq("mgmt", $.cidr, optional("host-reachable")),
        seq("dns", choice("hosts", "off")),
        seq("routing", choice("auto", "manual")),
      ),

    // ── Profile ─────────────────────────────────────
    profile_definition: ($) =>
      seq("profile", $.identifier, "{", repeat($._node_property), "}"),

    // ── Node ────────────────────────────────────────
    node_definition: ($) =>
      prec.right(
        seq(
          "node",
          $._name,
          optional(seq(":", $.profile_list)),
          optional($.node_image),
          optional($.node_body),
        ),
      ),

    profile_list: ($) => seq($._name, repeat(seq(",", $._name))),

    node_body: ($) => seq("{", repeat($._node_content), "}"),

    _node_content: ($) =>
      choice($._node_property, $.for_loop, $.if_block),

    _node_property: ($) =>
      choice(
        $.forward_property,
        $.sysctl_property,
        $.loopback_property,
        $.route_property,
        $.firewall_block,
        $.nat_block,
        $.vrf_block,
        $.wireguard_block,
        $.vxlan_block,
        $.dummy_block,
        $.macvlan_block,
        $.ipvlan_block,
        $.wifi_block,
        $.run_property,
        $.image_property,
        $.container_property,
      ),

    forward_property: ($) => seq("forward", choice("ipv4", "ipv6")),

    sysctl_property: ($) => seq("sysctl", $.string, $.string),

    loopback_property: ($) =>
      seq("lo", choice($._value, seq("pool", $.identifier))),

    route_property: ($) =>
      seq("route", $.route_destination, $.route_params),

    route_destination: ($) =>
      choice("default", $._value, $.list),

    route_params: ($) =>
      repeat1(
        choice(
          seq("via", $._value),
          seq("dev", $.identifier),
          seq("metric", $.integer),
        ),
      ),

    // ── Firewall ────────────────────────────────────
    firewall_block: ($) =>
      seq(
        "firewall",
        "policy",
        choice("accept", "drop", "reject"),
        "{",
        repeat($.firewall_rule),
        "}",
      ),

    firewall_rule: ($) =>
      seq(
        choice("accept", "drop", "reject"),
        optional($.match_expression),
      ),

    match_expression: ($) =>
      repeat1(
        choice(
          seq(choice("src", "dst"), $._value),
          seq(choice("tcp", "udp"), choice("dport", "sport"), $._value),
          seq(choice("icmp", "icmpv6"), optional($._value)),
          seq("ct", $.ct_states),
          seq("mark", $._value),
        ),
      ),

    ct_states: ($) => seq($.identifier, repeat(seq(",", $.identifier))),

    // ── NAT ─────────────────────────────────────────
    nat_block: ($) =>
      seq("nat", "{", repeat($._nat_content), "}"),

    _nat_content: ($) => choice($.nat_rule, $.for_loop),

    nat_rule: ($) =>
      choice(
        seq("masquerade", optional(seq("src", $._value))),
        seq("dnat", optional(seq("dst", $._value)), "to", $._value),
        seq("snat", optional(seq("src", $._value)), "to", $._value),
        seq("translate", $._value, "to", $._value),
      ),

    // ── VRF / WireGuard / VXLAN / Dummy / macvlan / ipvlan / wifi ──
    vrf_block: ($) =>
      seq("vrf", $.identifier, "table", $.integer, optional($.generic_block)),

    wireguard_block: ($) =>
      seq("wireguard", $.identifier, repeat($.inline_property), $.generic_block),

    vxlan_block: ($) =>
      seq("vxlan", $.identifier, repeat($.inline_property), $.generic_block),

    dummy_block: ($) =>
      seq("dummy", $.identifier, $.generic_block),

    macvlan_block: ($) =>
      seq("macvlan", $.identifier, repeat($.inline_property), $.generic_block),

    ipvlan_block: ($) =>
      seq("ipvlan", $.identifier, repeat($.inline_property), $.generic_block),

    wifi_block: ($) =>
      seq("wifi", $.identifier, repeat($.inline_property), $.generic_block),

    // `parent "enp3s0" mode l3` / `mode ap` before the block.
    inline_property: ($) =>
      choice(
        seq("parent", $._value),
        seq("mode", $.identifier),
      ),

    // ── Container properties ────────────────────────
    image_property: ($) => seq("image", $.string),

    // header form: `node r image "alpine" cmd "sleep infinity"`
    node_image: ($) =>
      prec.right(seq("image", $.string, optional(seq("cmd", choice($.string, $.list))))),

    container_property: ($) =>
      choice(
        seq("cpu", $._value),
        seq("memory", $._value),
        "privileged",
        seq(choice("cap-add", "cap-drop"), $.list),
        seq(choice("entrypoint", "hostname", "workdir"), $.string),
        seq("labels", $.list),
        seq("pull", $.identifier),
        seq("exec", $.string),
        seq("cmd", choice($.string, $.list)),
        seq("healthcheck", $.string, optional($.generic_block)),
        seq(choice("healthcheck-interval", "healthcheck-timeout"), $.duration),
        seq("startup-delay", $.duration),
        seq(choice("env-file", "overlay"), $.string),
        seq("config", $.string, $.string),
        seq("depends-on", $.list),
        seq("env", $.list),
        seq("volumes", $.list),
      ),

    run_property: ($) =>
      seq("run", optional("background"), choice($.string, $.list), optional("background")),

    // ── Link ────────────────────────────────────────
    link_definition: ($) =>
      seq(
        "link",
        $.endpoint,
        "--",
        $.endpoint,
        optional(seq(":", $.identifier)),
        optional($.link_body),
      ),

    link_body: ($) => seq("{", repeat($._link_item), "}"),

    _link_item: ($) =>
      choice(
        $.address_pair,
        // single-CIDR shorthand: `10.0.${i}.0/31` → both ends from one /31
        $.link_subnet,
        seq("subnet", $._value),
        seq("pool", choice($.identifier, "auto")),
        seq("mtu", $.integer),
        $.impairment_properties,
        $.directional_impairment,
        $.rate_properties,
      ),

    address_pair: ($) => seq($._value, "--", $._value),

    link_subnet: ($) => choice($.cidr, $.ipv6_cidr, $.interpolation, $.function_cidr),

    // ── Network ─────────────────────────────────────
    network_definition: ($) =>
      seq("network", $.identifier, "{", repeat($._network_item), "}"),

    _network_item: ($) =>
      choice(
        seq("members", $.list),
        "vlan-filtering",
        seq("mtu", $.integer),
        seq("subnet", $._value),
        seq("vlan", $.integer, optional($.string)),
        $.port_definition,
        $.network_impair,
        $.for_loop,
      ),

    port_definition: ($) =>
      seq(
        "port",
        choice($.endpoint, $._name),
        optional(seq("{", repeat($._port_item), "}")),
      ),

    _port_item: ($) =>
      choice(
        $._value,
        seq("pvid", $.integer),
        seq("vlans", $.list),
        "tagged",
        "untagged",
      ),

    // Per-pair impairment matrix: `impair a -- b { delay … rate-cap … }`.
    network_impair: ($) =>
      seq(
        "impair",
        $._name,
        "--",
        $._name,
        "{",
        repeat(choice($.impairment_properties, seq("rate-cap", $.rate))),
        "}",
      ),

    // ── Impairment / Rate ───────────────────────────
    impairment_statement: ($) =>
      seq("impair", $.endpoint, $.impairment_properties),

    rate_statement: ($) =>
      seq("rate", $.endpoint, $.rate_properties),

    impairment_properties: ($) =>
      prec.left(
        repeat1(
          choice(
            seq("delay", $._value),
            seq("jitter", $._value),
            seq("loss", $._value),
            seq("corrupt", $._value),
            seq("reorder", $._value),
            seq("rate", $._value),
            seq("duplicate", $._value),
            seq("delay-correlation", $._value),
            seq("loss-correlation", $._value),
            seq("limit", $._value),
          ),
        ),
      ),

    directional_impairment: ($) =>
      seq(choice("->", "<-"), $.impairment_properties),

    rate_properties: ($) =>
      prec.left(
        repeat1(
          seq(choice("egress", "ingress"), $._value, optional(seq("burst", $._value))),
        ),
      ),

    // ── Defaults / Pool / Pattern ───────────────────
    defaults_definition: ($) =>
      seq("defaults", $.identifier, $.generic_block),

    pool_definition: ($) =>
      seq("pool", $.identifier, $.cidr, "/", $.integer),

    pattern_definition: ($) =>
      seq(
        choice("mesh", "ring", "star"),
        $.identifier,
        "{",
        repeat($._pattern_item),
        "}",
      ),

    _pattern_item: ($) =>
      choice(
        seq("node", $.list),
        seq("count", $.integer),
        seq("pool", $.identifier),
        seq("profile", $.identifier),
        seq("hub", $.identifier),
        seq("spokes", $.list),
      ),

    // ── Validate / Scenario / Benchmark ─────────────
    validate_block: ($) =>
      seq("validate", "{", repeat($.assertion), "}"),

    assertion: ($) =>
      choice(
        seq(choice("reach", "no-reach"), $._name, $._name),
        seq(
          "tcp-connect",
          $._name,
          $._name,
          $.integer,
          repeat(
            choice(
              seq("timeout", $.duration),
              seq("retries", $.integer),
              seq("interval", $.duration),
            ),
          ),
        ),
        seq(
          "latency-under",
          $._name,
          $._name,
          $.duration,
          optional(seq("samples", $.integer)),
        ),
        seq(
          "route-has",
          $._name,
          $._value,
          optional(seq("via", $._value)),
          optional(seq("dev", $.identifier)),
        ),
        seq("dns-resolves", $._name, $._value, $._value),
      ),

    scenario_block: ($) =>
      seq("scenario", $.string, "{", repeat($.scenario_step), "}"),

    scenario_step: ($) =>
      seq("at", optional("+"), $.duration, "{", repeat($.scenario_action), "}"),

    scenario_action: ($) =>
      choice(
        seq("down", $.endpoint),
        seq("up", $.endpoint),
        seq("clear", $.endpoint),
        seq("validate", "{", repeat($.assertion), "}"),
        seq("exec", $._name, repeat1($.string)),
        seq("log", $.string),
      ),

    benchmark_block: ($) =>
      seq("benchmark", $.string, "{", repeat($.benchmark_test), "}"),

    benchmark_test: ($) =>
      seq(
        choice("iperf3", "ping"),
        $._name,
        $._name,
        optional(seq("{", repeat($.benchmark_property), "}")),
      ),

    benchmark_property: ($) =>
      choice(
        seq("duration", $.duration),
        seq("streams", $.integer),
        "udp",
        seq("count", $.integer),
        seq("assert", $.identifier, choice("above", "below"), $._value),
      ),

    // ── Control flow ────────────────────────────────
    let_binding: ($) => seq("let", $.identifier, "=", $._value),

    param_definition: ($) =>
      seq("param", $.identifier, optional(seq("default", $._value))),

    for_loop: ($) =>
      seq("for", $.identifier, "in", $.for_range, "{", repeat($._statement_or_prop), "}"),

    for_range: ($) =>
      choice(
        seq(
          choice($.integer, $.interpolation),
          "..",
          choice($.integer, $.interpolation),
        ),
        $.list,
      ),

    if_block: ($) =>
      seq("if", $.condition, "{", repeat($._statement_or_prop), "}"),

    condition: ($) =>
      seq(
        $._value,
        choice("==", "!=", "<", ">", "<=", ">="),
        $._value,
        repeat(seq(choice("&&", "||"), $._value, choice("==", "!=", "<", ">", "<=", ">="), $._value)),
      ),

    _statement_or_prop: ($) => choice($._statement, $._node_property, $.nat_rule, $.network_impair),

    site_block: ($) =>
      seq("site", $.identifier, optional($.string), "{", repeat($._statement), "}"),

    // ── Generic block (for VRF, WireGuard, etc.) ────
    generic_block: ($) =>
      seq("{", repeat($._generic_item), "}"),

    _generic_item: ($) =>
      choice(
        $.known_property,
        seq($.identifier, $._value),
        seq($.identifier, $.list),
        seq($.identifier, $.generic_block),
        $.route_property,
        // bare address lines: `10.0.0.1/24`, `fd00::1/64`, `${addr}/24`
        choice($.cidr, $.ipv6_cidr, $.interpolation, $.function_cidr),
        $.for_loop,
      ),

    // Properties the language defines for wireguard / vxlan / wifi / vrf
    // / healthcheck blocks (kept explicit so highlighters can name them).
    known_property: ($) =>
      choice(
        seq("interfaces", $.list),
        seq("listen", $.integer),
        seq("key", $._value),
        seq("peers", $.list),
        seq("fwmark", $.integer),
        seq("address", $._value),
        seq("vni", $.integer),
        seq("local", $._value),
        seq("remote", $._value),
        seq("underlay", $.identifier),
        seq("ssid", $.string),
        seq("channel", $.integer),
        seq("wpa2", $.string),
        seq("mesh-id", $.string),
        seq("interval", $.duration),
        seq("timeout", $.duration),
        seq("retries", $.integer),
      ),

    // ── Expressions and literals ────────────────────
    _value: ($) =>
      choice(
        $.cidr,
        $.ipv6_cidr,
        $.ipv6_address,
        $.ipv4_address,
        $.duration,
        $.rate,
        $.percent,
        $.float,
        $.integer,
        $.string,
        $.interpolation,
        $.function_cidr,
        $.function_call,
        $._name,
      ),

    // `host(${lan}, 1)/24` — a computed address with a prefix.
    function_cidr: ($) => seq($.function_call, "/", $.integer),

    ipv4_address: ($) =>
      token(/[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+/),

    // Mirrors the Rust lexer (crates/nlink-lab/src/parser/nll/lexer.rs):
    // compressed (`::` anywhere, optional trailing IPv4), fully expanded
    // 8-group, or 6 groups + IPv4. A token is IPv6 only with a `::` or
    // seven colons, so `node:iface` endpoints never match.
    ipv6_address: ($) =>
      token(/(([0-9a-fA-F]{1,4}(:[0-9a-fA-F]{1,4})*)?::(([0-9a-fA-F]{1,4}:)*([0-9a-fA-F]{1,4}|[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+))?|[0-9a-fA-F]{1,4}(:[0-9a-fA-F]{1,4}){7}|[0-9a-fA-F]{1,4}(:[0-9a-fA-F]{1,4}){5}:[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+)/),

    // `spine${s}` is one `interpolation` token, so a name is one token.
    // `*-black` is a glob (network members only, but harmless elsewhere).
    _name: ($) => choice($.identifier, $.interpolation, $.glob),

    endpoint: ($) => seq($._name, ":", $._name),

    function_call: ($) =>
      seq(
        $.identifier,
        "(",
        optional(seq($._value, repeat(seq(",", $._value)))),
        ")",
      ),

    list: ($) =>
      seq(
        "[",
        optional(seq($._list_item, repeat(seq(",", $._list_item)), optional(","))),
        "]",
      ),

    _list_item: ($) => choice($._value, $.endpoint, $.list_for),

    // `[for i in 1..4 : r${i}:mgmt0]`
    list_for: ($) =>
      seq("for", $.identifier, "in", $.for_range, ":", choice($.endpoint, $._value)),

    // ── Tokens ──────────────────────────────────────
    identifier: ($) => /[a-zA-Z_][a-zA-Z0-9_-]*/,

    glob: ($) => token(/(\*[a-zA-Z0-9_*-]*|[a-zA-Z_][a-zA-Z0-9_-]*\*[a-zA-Z0-9_*-]*)/),

    string: ($) => /"[^"]*"/,

    integer: ($) => /[0-9]+/,

    float: ($) => token(/[0-9]+\.[0-9]+/),

    cidr: ($) =>
      token(
        /[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+\/[0-9]+/,
      ),

    ipv6_cidr: ($) =>
      token(
        /(([0-9a-fA-F]{1,4}(:[0-9a-fA-F]{1,4})*)?::(([0-9a-fA-F]{1,4}:)*([0-9a-fA-F]{1,4}|[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+))?|[0-9a-fA-F]{1,4}(:[0-9a-fA-F]{1,4}){7}|[0-9a-fA-F]{1,4}(:[0-9a-fA-F]{1,4}){5}:[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+)\/[0-9]+/,
      ),

    duration: ($) => token(/[0-9]+(\.[0-9]+)?(ns|us|ms|s|m|h)/),

    rate: ($) =>
      token(/[0-9]+(\.[0-9]+)?(bit|bps|kbit|kbps|mbit|mbps|gbit|gbps|kbyte|mbyte|gbyte)/),

    percent: ($) => token(/[0-9]+(\.[0-9]+)?%/),

    // A token containing `${…}`: `spine${s}`, `10.255.0.${s}/32`,
    // `${(i + 1) % 12}`. Mirrors the Rust lexer, which keeps
    // interpolated text as one unit until lowering.
    interpolation: ($) =>
      token(
        /([A-Za-z0-9_\/%-]+(\.[A-Za-z0-9_\/%-]+)*\.?)?(\$\{[^}]+\}(\.?[A-Za-z0-9_\/%-]+)*\.?)+/,
      ),

    line_comment: ($) => /#[^\n]*/,

    block_comment: ($) => token(seq("/*", /[^*]*\*+([^/*][^*]*\*+)*/, "/")),
  },
});
