/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

// NLL (nlink-lab Language) grammar for tree-sitter.
//
// NLL is a topology definition DSL with context-sensitive keywords,
// interpolation, typed literals (CIDR, duration, rate, percent),
// and block-based structure.

module.exports = grammar({
  name: "nll",

  extras: ($) => [/\s/, $.line_comment, $.block_comment],

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
        seq("mgmt", $.cidr),
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
          optional($.image_property),
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
      seq("lo", choice($.cidr, seq("pool", $.identifier))),

    route_property: ($) =>
      seq("route", $.route_destination, $.route_params),

    route_destination: ($) =>
      choice("default", $.cidr, $.list),

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
        choice("accept", "drop"),
        "{",
        repeat($.firewall_rule),
        "}",
      ),

    firewall_rule: ($) =>
      seq(
        choice("accept", "drop"),
        optional($.match_expression),
      ),

    match_expression: ($) =>
      repeat1(
        choice(
          seq(choice("src", "dst"), $._value),
          seq(choice("tcp", "udp"), choice("dport", "sport"), $._value),
          seq("icmp", "type", $._value),
          seq("ct", "state", $._value),
          seq("mark", $._value),
        ),
      ),

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

    // ── VRF / WireGuard / VXLAN / Dummy ─────────────

    vrf_block: ($) =>
      seq("vrf", $.identifier, "table", $.integer, optional($.generic_block)),

    wireguard_block: ($) =>
      seq("wireguard", $.identifier, $.generic_block),

    vxlan_block: ($) =>
      seq("vxlan", $.identifier, $.generic_block),

    dummy_block: ($) =>
      seq("dummy", $.identifier, $.generic_block),

    macvlan_block: ($) =>
      seq("macvlan", $.identifier, $.generic_block),

    ipvlan_block: ($) =>
      seq("ipvlan", $.identifier, $.generic_block),

    wifi_block: ($) =>
      seq("wifi", $.identifier, $.generic_block),

    // ── Container properties ────────────────────────

    image_property: ($) =>
      seq("image", $.string, optional(seq("cmd", choice($.string, $.list)))),

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
        seq("healthcheck", $.string, optional($.generic_block)),
        seq("startup-delay", $.duration),
        seq(choice("env-file", "overlay"), $.string),
        seq("config", $.string, $.string),
        seq("depends-on", $.list),
        seq("env", $.list),
        seq("volumes", $.list),
      ),

    run_property: ($) =>
      seq("run", optional("background"), choice($.string, $.list)),

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
        seq("subnet", $.cidr),
        seq("pool", choice($.identifier, "auto")),
        seq("mtu", $.integer),
        $.impairment_properties,
        $.directional_impairment,
        $.rate_properties,
      ),

    address_pair: ($) => seq($._value, "--", $._value),

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
      ),

    port_definition: ($) =>
      seq("port", $.endpoint, "{", repeat($._port_item), "}"),

    _port_item: ($) =>
      choice(
        $.cidr,
        seq("pvid", $.integer),
        seq("vlans", $.list),
        "tagged",
        "untagged",
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
            seq("delay", $.duration),
            seq("jitter", $.duration),
            seq("loss", $.percent),
            seq("corrupt", $.percent),
            seq("reorder", $.percent),
            seq("rate", $.rate),
          ),
        ),
      ),

    directional_impairment: ($) =>
      seq(choice("->", "<-"), $.impairment_properties),

    rate_properties: ($) =>
      prec.left(repeat1(seq(choice("egress", "ingress"), $.rate))),

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
        seq(choice("reach", "no-reach"), $.identifier, $.identifier),
        seq(
          "tcp-connect",
          $.identifier,
          $.identifier,
          $.integer,
          optional(seq("timeout", $.duration)),
        ),
        seq(
          "latency-under",
          $.identifier,
          $.identifier,
          $.duration,
          optional(seq("samples", $.integer)),
        ),
        seq(
          "route-has",
          $.identifier,
          $._value,
          optional(seq("via", $._value)),
          optional(seq("dev", $.identifier)),
        ),
        seq("dns-resolves", $.identifier, $._value, $._value),
      ),

    scenario_block: ($) =>
      seq("scenario", $.string, "{", repeat($.scenario_step), "}"),

    scenario_step: ($) =>
      seq("at", $.duration, "{", repeat($.scenario_action), "}"),

    scenario_action: ($) =>
      choice(
        seq("down", $.endpoint),
        seq("up", $.endpoint),
        seq("clear", $.endpoint),
        seq("validate", "{", repeat($.assertion), "}"),
        seq("exec", $.identifier, repeat1($.string)),
        seq("log", $.string),
      ),

    benchmark_block: ($) =>
      seq("benchmark", $.string, "{", repeat($.benchmark_test), "}"),

    benchmark_test: ($) =>
      seq(
        choice("iperf3", "ping"),
        $.identifier,
        $.identifier,
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
        seq($.integer, "..", $.integer),
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

    _statement_or_prop: ($) => choice($._statement, $._node_property),

    site_block: ($) =>
      seq("site", $.identifier, optional($.string), "{", repeat($._statement), "}"),

    // ── Generic block (for VRF, WireGuard, etc.) ────

    generic_block: ($) =>
      seq("{", repeat($._generic_item), "}"),

    _generic_item: ($) =>
      choice(
        seq($.identifier, $._value),
        seq($.identifier, $.list),
        seq($.identifier, $.generic_block),
        $.route_property,
        $.cidr,
        $.for_loop,
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
        $.integer,
        $.string,
        $.interpolation,
        $.function_call,
        $._name,
      ),

    ipv4_address: ($) =>
      token(/[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+/),

    ipv6_address: ($) =>
      token(/[0-9a-fA-F]*::[0-9a-fA-F:]*/),

    _name: ($) =>
      prec.right(
        repeat1(choice($.identifier, $.interpolation)),
      ),

    endpoint: ($) => seq($._name, ":", $._name),

    function_call: ($) =>
      seq(
        $.identifier,
        "(",
        optional(seq($._value, repeat(seq(",", $._value)))),
        ")",
      ),

    list: ($) =>
      seq("[", optional(seq($._list_item, repeat(seq(",", $._list_item)))), "]"),

    _list_item: ($) => choice($._value, $.endpoint),

    // ── Tokens ──────────────────────────────────────

    identifier: ($) => /[a-zA-Z_][a-zA-Z0-9_-]*/,

    string: ($) => /"[^"]*"/,

    integer: ($) => /[0-9]+/,

    cidr: ($) =>
      token(
        /[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+\/[0-9]+/,
      ),

    ipv6_cidr: ($) =>
      token(
        /[0-9a-fA-F]*::[0-9a-fA-F:]*\/[0-9]+/,
      ),

    duration: ($) => token(/[0-9]+(ms|s|m|h)/),

    rate: ($) => token(/[0-9]+(kbit|mbit|gbit|kbps|mbps|gbps)/),

    percent: ($) => token(/[0-9]+(\.[0-9]+)?%/),

    interpolation: ($) => /\$\{[^}]+\}/,

    line_comment: ($) => /#[^\n]*/,

    block_comment: ($) => token(seq("/*", /[^*]*\*+([^/*][^*]*\*+)*/, "/")),
  },
});
