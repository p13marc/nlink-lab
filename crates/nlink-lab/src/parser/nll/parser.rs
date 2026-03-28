//! NLL parser — converts token stream into AST.

use super::ast;
use super::lexer::Spanned;
use crate::error::Result;

/// Parse a token stream into an NLL AST.
pub fn parse_tokens(tokens: &[Spanned], _source: &str) -> Result<ast::File> {
    let mut pos = 0;
    let lab = parse_lab_decl(tokens, &mut pos)?;
    let mut statements = Vec::new();

    while pos < tokens.len() {
        skip_newlines(tokens, &mut pos);
        if pos >= tokens.len() {
            break;
        }
        statements.push(parse_statement(tokens, &mut pos)?);
    }

    Ok(ast::File { lab, statements })
}

// ─── Helpers ──────────────────────────────────────────────

use super::lexer::Token;

/// Create a parse error with span information from the current token.
fn err(tokens: &[Spanned], pos: usize, msg: String) -> crate::Error {
    if pos < tokens.len() {
        let span = &tokens[pos].span;
        crate::Error::NllParse(format!("{msg} [at byte {start}]", start = span.start))
    } else {
        crate::Error::NllParse(msg)
    }
}

fn skip_newlines(tokens: &[Spanned], pos: &mut usize) {
    while *pos < tokens.len() && tokens[*pos].token == Token::Newline {
        *pos += 1;
    }
}

fn expect(tokens: &[Spanned], pos: &mut usize, expected: &Token) -> Result<()> {
    if *pos >= tokens.len() {
        return Err(err(tokens, *pos, format!("unexpected end of input, expected {expected}")));
    }
    if &tokens[*pos].token != expected {
        return Err(err(tokens, *pos, format!("expected {expected}, found {}", tokens[*pos].token)));
    }
    *pos += 1;
    Ok(())
}

fn expect_ident(tokens: &[Spanned], pos: &mut usize) -> Result<String> {
    if *pos >= tokens.len() {
        return Err(err(tokens, *pos, "unexpected end of input, expected identifier".into()));
    }
    // Accept both Ident and keywords-as-identifiers (e.g. `let delay = ...`)
    if let Some(name) = token_as_ident(&tokens[*pos].token) {
        *pos += 1;
        Ok(name)
    } else {
        Err(err(tokens, *pos, format!("expected identifier, found {}", tokens[*pos].token)))
    }
}

/// Extract an identifier string from a token, treating keywords as identifiers
/// in contexts where they are used as names.
fn token_as_ident(token: &Token) -> Option<String> {
    match token {
        Token::Ident(s) => Some(s.clone()),
        // Allow keywords to be used as identifiers
        Token::Delay => Some("delay".into()),
        Token::Jitter => Some("jitter".into()),
        Token::Loss => Some("loss".into()),
        Token::Rate => Some("rate".into()),
        Token::Corrupt => Some("corrupt".into()),
        Token::Reorder => Some("reorder".into()),
        Token::Forward => Some("forward".into()),
        Token::Route => Some("route".into()),
        Token::Lo => Some("lo".into()),
        Token::Mtu => Some("mtu".into()),
        Token::Table => Some("table".into()),
        Token::Policy => Some("policy".into()),
        Token::Accept => Some("accept".into()),
        Token::Drop => Some("drop".into()),
        Token::Reject => Some("reject".into()),
        Token::Key => Some("key".into()),
        Token::Auto => Some("auto".into()),
        Token::Listen => Some("listen".into()),
        Token::Address => Some("address".into()),
        Token::Port => Some("port".into()),
        Token::Local => Some("local".into()),
        Token::Remote => Some("remote".into()),
        Token::Default => Some("default".into()),
        Token::Dev => Some("dev".into()),
        Token::Metric => Some("metric".into()),
        Token::Egress => Some("egress".into()),
        Token::Ingress => Some("ingress".into()),
        Token::Burst => Some("burst".into()),
        Token::Env => Some("env".into()),
        Token::Volumes => Some("volumes".into()),
        Token::Runtime => Some("runtime".into()),
        Token::Parent => Some("parent".into()),
        _ => None,
    }
}

fn expect_string(tokens: &[Spanned], pos: &mut usize) -> Result<String> {
    if *pos >= tokens.len() {
        return Err(err(tokens, *pos, "unexpected end of input, expected string".into()));
    }
    match &tokens[*pos].token {
        Token::String(s) => {
            let s = s.clone();
            *pos += 1;
            Ok(s)
        }
        _ => Err(err(tokens, *pos, format!("expected string, found {}", tokens[*pos].token))),
    }
}

fn expect_int(tokens: &[Spanned], pos: &mut usize) -> Result<i64> {
    if *pos >= tokens.len() {
        return Err(err(tokens, *pos, "unexpected end of input, expected integer".into()));
    }
    match &tokens[*pos].token {
        Token::Int(s) => {
            let v = s.parse::<i64>().map_err(|e| {
                err(tokens, *pos, format!("invalid integer '{s}': {e}"))
            })?;
            *pos += 1;
            Ok(v)
        }
        _ => Err(err(tokens, *pos, format!("expected integer, found {}", tokens[*pos].token))),
    }
}

fn at(tokens: &[Spanned], pos: usize) -> Option<&Token> {
    tokens.get(pos).map(|s| &s.token)
}

fn check(tokens: &[Spanned], pos: usize, expected: &Token) -> bool {
    at(tokens, pos) == Some(expected)
}

/// Consume a token if it matches, returning true.
fn eat(tokens: &[Spanned], pos: &mut usize, expected: &Token) -> bool {
    if check(tokens, *pos, expected) {
        *pos += 1;
        true
    } else {
        false
    }
}

/// Parse a compound name that may contain interpolation: `spine${i}` → `"spine${i}"`.
///
/// Names must start with an identifier or interpolation, not a bare integer.
/// Integers are allowed as continuations (e.g. `spine1`, `leaf${i}2`).
fn parse_name(tokens: &[Spanned], pos: &mut usize) -> Result<String> {
    let mut name = String::new();
    let start = *pos;
    let mut started = false;

    loop {
        if *pos >= tokens.len() {
            break;
        }
        match &tokens[*pos].token {
            Token::Ident(s) => {
                name.push_str(s);
                *pos += 1;
                started = true;
            }
            Token::Interp(s) => {
                name.push_str(s);
                *pos += 1;
                started = true;
            }
            Token::Int(s) if started => {
                // Integers only allowed after an ident/interp (e.g. `spine1`)
                name.push_str(s);
                *pos += 1;
            }
            _ => break,
        }
    }

    if name.is_empty() {
        return Err(err(tokens, *pos, format!(
            "expected name at position {}",
            if start < tokens.len() {
                format!("(found {})", tokens[start].token)
            } else {
                "end of input".into()
            }
        )));
    }

    Ok(name)
}

/// Parse an endpoint reference: `node:iface` (may contain interpolation).
fn parse_endpoint(tokens: &[Spanned], pos: &mut usize) -> Result<(String, String)> {
    let node = parse_name(tokens, pos)?;
    expect(tokens, pos, &Token::Colon)?;
    let iface = parse_name(tokens, pos)?;
    Ok((node, iface))
}

/// Parse a string that can be a string literal, ident, cidr, ipv4, duration, rate, percent, int, or interp.
fn parse_value(tokens: &[Spanned], pos: &mut usize) -> Result<String> {
    if *pos >= tokens.len() {
        return Err(err(tokens, *pos, 
            "unexpected end of input, expected value".into(),
        ));
    }
    let val = match &tokens[*pos].token {
        Token::String(s) => s.clone(),
        Token::Ident(s) => s.clone(),
        Token::Int(s) => s.clone(),
        Token::Cidr(s) => s.clone(),
        Token::Ipv4Addr(s) => s.clone(),
        Token::Duration(s) => s.clone(),
        Token::RateLit(s) => s.clone(),
        Token::Percent(s) => s.clone(),
        Token::Interp(s) => s.clone(),
        Token::Default => "default".into(),
        Token::Auto => "auto".into(),
        other => {
            return Err(err(tokens, *pos, format!(
                "expected value, found {other}"
            )));
        }
    };
    *pos += 1;
    Ok(val)
}

// ─── Lab Declaration ──────────────────────────────────────

fn parse_lab_decl(tokens: &[Spanned], pos: &mut usize) -> Result<ast::LabDecl> {
    skip_newlines(tokens, pos);
    expect(tokens, pos, &Token::Lab)?;

    let name = expect_string(tokens, pos)?;
    let mut description = None;
    let mut prefix = None;
    let mut runtime = None;

    // Parse optional inline runtime before block
    if eat(tokens, pos, &Token::Runtime) {
        runtime = Some(expect_string(tokens, pos)?);
    }

    if eat(tokens, pos, &Token::LBrace) {
        loop {
            skip_newlines(tokens, pos);
            if eat(tokens, pos, &Token::RBrace) {
                break;
            }
            match at(tokens, *pos) {
                Some(Token::Description) => {
                    *pos += 1;
                    description = Some(expect_string(tokens, pos)?);
                }
                Some(Token::Prefix) => {
                    *pos += 1;
                    prefix = Some(expect_string(tokens, pos)?);
                }
                Some(Token::Runtime) => {
                    *pos += 1;
                    runtime = Some(expect_string(tokens, pos)?);
                }
                Some(other) => {
                    return Err(err(tokens, *pos, format!(
                        "unexpected {other} in lab block"
                    )));
                }
                None => {
                    return Err(err(tokens, *pos,
                        "unexpected end of input in lab block".into(),
                    ));
                }
            }
        }
    }

    Ok(ast::LabDecl {
        name,
        description,
        prefix,
        runtime,
    })
}

// ─── Statements ───────────────────────────────────────────

fn parse_statement(tokens: &[Spanned], pos: &mut usize) -> Result<ast::Statement> {
    skip_newlines(tokens, pos);
    if *pos >= tokens.len() {
        return Err(err(tokens, *pos, 
            "unexpected end of input, expected statement".into(),
        ));
    }

    match &tokens[*pos].token {
        Token::Profile => parse_profile(tokens, pos).map(ast::Statement::Profile),
        Token::Node => parse_node(tokens, pos).map(ast::Statement::Node),
        Token::Link => parse_link(tokens, pos).map(ast::Statement::Link),
        Token::Network => parse_network(tokens, pos).map(ast::Statement::Network),
        Token::Impair => parse_impair_stmt(tokens, pos).map(ast::Statement::Impair),
        Token::Rate => parse_rate_stmt(tokens, pos).map(ast::Statement::Rate),
        Token::Let => parse_let(tokens, pos).map(ast::Statement::Let),
        Token::For => parse_for(tokens, pos).map(ast::Statement::For),
        other => Err(err(tokens, *pos, format!(
            "expected statement (profile, node, link, network, impair, rate, let, for), found {other}"
        ))),
    }
}

// ─── Profile ──────────────────────────────────────────────

fn parse_profile(tokens: &[Spanned], pos: &mut usize) -> Result<ast::ProfileDef> {
    expect(tokens, pos, &Token::Profile)?;
    let name = expect_ident(tokens, pos)?;
    let props = parse_node_block(tokens, pos)?;
    Ok(ast::ProfileDef { name, props })
}

// ─── Node ─────────────────────────────────────────────────

fn parse_node(tokens: &[Spanned], pos: &mut usize) -> Result<ast::NodeDef> {
    expect(tokens, pos, &Token::Node)?;
    let name = parse_name(tokens, pos)?;

    let profile = if eat(tokens, pos, &Token::Colon) {
        Some(expect_ident(tokens, pos)?)
    } else {
        None
    };

    // Parse inline image/cmd before the block
    let mut image = None;
    let mut cmd = None;
    let mut env = Vec::new();
    let mut volumes = Vec::new();
    if eat(tokens, pos, &Token::Image) {
        image = Some(expect_string(tokens, pos)?);
        if eat(tokens, pos, &Token::Cmd) {
            if check(tokens, *pos, &Token::LBracket) {
                cmd = Some(parse_string_list(tokens, pos)?);
            } else {
                cmd = Some(vec![expect_string(tokens, pos)?]);
            }
        }
    }

    let props = if check(tokens, *pos, &Token::LBrace) {
        let mut props = Vec::new();
        expect(tokens, pos, &Token::LBrace)?;
        loop {
            skip_newlines(tokens, pos);
            if eat(tokens, pos, &Token::RBrace) {
                break;
            }
            match at(tokens, *pos) {
                Some(Token::Image) => {
                    *pos += 1;
                    image = Some(expect_string(tokens, pos)?);
                }
                Some(Token::Cmd) => {
                    *pos += 1;
                    if check(tokens, *pos, &Token::LBracket) {
                        cmd = Some(parse_string_list(tokens, pos)?);
                    } else {
                        cmd = Some(vec![expect_string(tokens, pos)?]);
                    }
                }
                Some(Token::Env) => {
                    *pos += 1;
                    env = parse_string_list(tokens, pos)?;
                }
                Some(Token::Volumes) => {
                    *pos += 1;
                    volumes = parse_string_list(tokens, pos)?;
                }
                _ => {
                    props.push(parse_node_prop(tokens, pos)?);
                }
            }
        }
        props
    } else {
        Vec::new()
    };

    Ok(ast::NodeDef {
        name,
        profile,
        image,
        cmd,
        env,
        volumes,
        props,
    })
}

fn parse_node_block(tokens: &[Spanned], pos: &mut usize) -> Result<Vec<ast::NodeProp>> {
    expect(tokens, pos, &Token::LBrace)?;
    let mut props = Vec::new();

    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }
        props.push(parse_node_prop(tokens, pos)?);
    }

    Ok(props)
}

fn parse_node_prop(tokens: &[Spanned], pos: &mut usize) -> Result<ast::NodeProp> {
    match at(tokens, *pos) {
        Some(Token::Forward) => {
            *pos += 1;
            let version = match at(tokens, *pos) {
                Some(Token::Ipv4) => {
                    *pos += 1;
                    ast::IpVersion::Ipv4
                }
                Some(Token::Ipv6) => {
                    *pos += 1;
                    ast::IpVersion::Ipv6
                }
                other => {
                    return Err(err(tokens, *pos, format!(
                        "expected 'ipv4' or 'ipv6' after 'forward', found {}",
                        other.map_or("end of input".to_string(), |t| t.to_string())
                    )));
                }
            };
            Ok(ast::NodeProp::Forward(version))
        }
        Some(Token::Sysctl) => {
            *pos += 1;
            let key = expect_string(tokens, pos)?;
            let value = expect_string(tokens, pos)?;
            Ok(ast::NodeProp::Sysctl(key, value))
        }
        Some(Token::Lo) => {
            *pos += 1;
            let addr = parse_cidr_or_name(tokens, pos)?;
            Ok(ast::NodeProp::Lo(addr))
        }
        Some(Token::Route) => {
            *pos += 1;
            parse_route_def(tokens, pos).map(ast::NodeProp::Route)
        }
        Some(Token::Firewall) => {
            *pos += 1;
            parse_firewall_def(tokens, pos).map(ast::NodeProp::Firewall)
        }
        Some(Token::Vrf) => {
            *pos += 1;
            parse_vrf_def(tokens, pos).map(ast::NodeProp::Vrf)
        }
        Some(Token::Wireguard) => {
            *pos += 1;
            parse_wireguard_def(tokens, pos).map(ast::NodeProp::Wireguard)
        }
        Some(Token::Vxlan) => {
            *pos += 1;
            parse_vxlan_def(tokens, pos).map(ast::NodeProp::Vxlan)
        }
        Some(Token::Dummy) => {
            *pos += 1;
            parse_dummy_def(tokens, pos).map(ast::NodeProp::Dummy)
        }
        Some(Token::Run) => {
            *pos += 1;
            parse_run_def(tokens, pos).map(ast::NodeProp::Run)
        }
        Some(other) => Err(err(tokens, *pos, format!(
            "expected node property (forward, sysctl, lo, route, firewall, vrf, wireguard, vxlan, dummy, run), found {other}"
        ))),
        None => Err(err(tokens, *pos, 
            "unexpected end of input in node block".into(),
        )),
    }
}

fn parse_cidr_or_name(tokens: &[Spanned], pos: &mut usize) -> Result<String> {
    if *pos >= tokens.len() {
        return Err(err(tokens, *pos, 
            "unexpected end of input, expected CIDR or address".into(),
        ));
    }
    // May be CIDR, IP, or compound address with interpolation (e.g. 10.255.0.${i}/32)
    let mut val = String::new();
    loop {
        if *pos >= tokens.len() {
            break;
        }
        match &tokens[*pos].token {
            Token::Cidr(s) | Token::Ipv4Addr(s) => {
                val.push_str(s);
                *pos += 1;
                // After a full CIDR/IP, stop unless followed by interpolation-related tokens
                break;
            }
            Token::Interp(s) => {
                val.push_str(s);
                *pos += 1;
            }
            Token::Ident(s) => {
                val.push_str(s);
                *pos += 1;
            }
            Token::Int(s) => {
                val.push_str(s);
                *pos += 1;
            }
            Token::Dot => {
                val.push('.');
                *pos += 1;
            }
            Token::Slash => {
                val.push('/');
                *pos += 1;
            }
            _ => break,
        }
    }
    if val.is_empty() {
        return Err(err(tokens, *pos, format!(
            "expected CIDR or address, found {}",
            tokens[*pos].token
        )));
    }
    Ok(val)
}

// ─── Route ────────────────────────────────────────────────

fn parse_route_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::RouteDef> {
    // destination: "default" or CIDR
    let destination = if eat(tokens, pos, &Token::Default) {
        "default".to_string()
    } else {
        parse_cidr_or_name(tokens, pos)?
    };

    let mut via = None;
    let mut dev = None;
    let mut metric = None;

    // Parse optional route parameters on same line
    loop {
        match at(tokens, *pos) {
            Some(Token::Via) => {
                *pos += 1;
                via = Some(parse_cidr_or_name(tokens, pos)?);
            }
            Some(Token::Dev) => {
                *pos += 1;
                dev = Some(parse_name(tokens, pos)?);
            }
            Some(Token::Metric) => {
                *pos += 1;
                metric = Some(expect_int(tokens, pos)? as u32);
            }
            _ => break,
        }
    }

    Ok(ast::RouteDef {
        destination,
        via,
        dev,
        metric,
    })
}

// ─── Firewall ─────────────────────────────────────────────

fn parse_firewall_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::FirewallDef> {
    expect(tokens, pos, &Token::Policy)?;
    let policy = match at(tokens, *pos) {
        Some(Token::Accept) => {
            *pos += 1;
            "accept".to_string()
        }
        Some(Token::Drop) => {
            *pos += 1;
            "drop".to_string()
        }
        Some(Token::Reject) => {
            *pos += 1;
            "reject".to_string()
        }
        Some(Token::Ident(s)) => {
            let s = s.clone();
            *pos += 1;
            s
        }
        other => {
            return Err(err(tokens, *pos, format!(
                "expected firewall policy, found {}",
                other.map_or("end of input".to_string(), |t| t.to_string())
            )));
        }
    };

    let mut rules = Vec::new();
    expect(tokens, pos, &Token::LBrace)?;

    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }
        rules.push(parse_firewall_rule(tokens, pos)?);
    }

    Ok(ast::FirewallDef { policy, rules })
}

fn parse_firewall_rule(tokens: &[Spanned], pos: &mut usize) -> Result<ast::FirewallRuleDef> {
    let action = match at(tokens, *pos) {
        Some(Token::Accept) => {
            *pos += 1;
            "accept".to_string()
        }
        Some(Token::Drop) => {
            *pos += 1;
            "drop".to_string()
        }
        Some(Token::Reject) => {
            *pos += 1;
            "reject".to_string()
        }
        other => {
            return Err(err(tokens, *pos, format!(
                "expected firewall action (accept/drop/reject), found {}",
                other.map_or("end of input".to_string(), |t| t.to_string())
            )));
        }
    };

    // Parse match expression
    let match_expr = parse_match_expr(tokens, pos)?;

    Ok(ast::FirewallRuleDef { action, match_expr })
}

fn parse_match_expr(tokens: &[Spanned], pos: &mut usize) -> Result<String> {
    let mut expr = String::new();

    match at(tokens, *pos) {
        Some(Token::Ct) => {
            *pos += 1;
            expr.push_str("ct state ");
            // Parse comma-separated state list
            let state = parse_name(tokens, pos)?;
            expr.push_str(&state);
            while eat(tokens, pos, &Token::Comma) {
                let state = parse_name(tokens, pos)?;
                expr.push(',');
                expr.push_str(&state);
            }
        }
        Some(Token::Tcp) => {
            *pos += 1;
            expr.push_str("tcp ");
            expect(tokens, pos, &Token::Dport)?;
            expr.push_str("dport ");
            let port = expect_int(tokens, pos)?;
            expr.push_str(&port.to_string());
        }
        Some(Token::Udp) => {
            *pos += 1;
            expr.push_str("udp ");
            expect(tokens, pos, &Token::Dport)?;
            expr.push_str("dport ");
            let port = expect_int(tokens, pos)?;
            expr.push_str(&port.to_string());
        }
        Some(other) => {
            return Err(err(tokens, *pos, format!(
                "expected match expression (ct/tcp/udp), found {other}"
            )));
        }
        None => {
            return Err(err(tokens, *pos, 
                "unexpected end of input in firewall rule".into(),
            ));
        }
    }

    Ok(expr)
}

// ─── VRF ──────────────────────────────────────────────────

fn parse_vrf_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::VrfDef> {
    let name = expect_ident(tokens, pos)?;
    expect(tokens, pos, &Token::Table)?;
    let table = expect_int(tokens, pos)? as u32;

    let mut interfaces = Vec::new();
    let mut routes = Vec::new();

    expect(tokens, pos, &Token::LBrace)?;
    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }
        match at(tokens, *pos) {
            Some(Token::Interfaces) => {
                *pos += 1;
                interfaces = parse_ident_list(tokens, pos)?;
            }
            Some(Token::Route) => {
                *pos += 1;
                routes.push(parse_route_def(tokens, pos)?);
            }
            Some(other) => {
                return Err(err(tokens, *pos, format!(
                    "unexpected {other} in VRF block"
                )));
            }
            None => {
                return Err(err(tokens, *pos, 
                    "unexpected end of input in VRF block".into(),
                ));
            }
        }
    }

    Ok(ast::VrfDef {
        name,
        table,
        interfaces,
        routes,
    })
}

// ─── WireGuard ────────────────────────────────────────────

fn parse_wireguard_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::WireguardDef> {
    let name = expect_ident(tokens, pos)?;

    let mut key = None;
    let mut listen_port = None;
    let mut addresses = Vec::new();
    let mut peers = Vec::new();

    expect(tokens, pos, &Token::LBrace)?;
    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }
        match at(tokens, *pos) {
            Some(Token::Key) => {
                *pos += 1;
                key = Some(parse_value(tokens, pos)?);
            }
            Some(Token::Listen) => {
                *pos += 1;
                listen_port = Some(expect_int(tokens, pos)? as u16);
            }
            Some(Token::Address) => {
                *pos += 1;
                addresses.push(parse_cidr_or_name(tokens, pos)?);
            }
            Some(Token::Peers) => {
                *pos += 1;
                peers = parse_ident_list(tokens, pos)?;
            }
            Some(other) => {
                return Err(err(tokens, *pos, format!(
                    "unexpected {other} in wireguard block"
                )));
            }
            None => {
                return Err(err(tokens, *pos,
                    "unexpected end of input in wireguard block".into(),
                ));
            }
        }
    }

    Ok(ast::WireguardDef {
        name,
        key,
        listen_port,
        addresses,
        peers,
    })
}

// ─── VXLAN ────────────────────────────────────────────────

fn parse_vxlan_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::VxlanDef> {
    let name = expect_ident(tokens, pos)?;

    let mut vni = 0;
    let mut local = None;
    let mut remote = None;
    let mut port = None;
    let mut addresses = Vec::new();

    expect(tokens, pos, &Token::LBrace)?;
    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }
        match at(tokens, *pos) {
            Some(Token::Vni) => {
                *pos += 1;
                vni = expect_int(tokens, pos)? as u32;
            }
            Some(Token::Local) => {
                *pos += 1;
                local = Some(parse_cidr_or_name(tokens, pos)?);
            }
            Some(Token::Remote) => {
                *pos += 1;
                remote = Some(parse_cidr_or_name(tokens, pos)?);
            }
            Some(Token::Port) => {
                *pos += 1;
                port = Some(expect_int(tokens, pos)? as u16);
            }
            Some(Token::Address) => {
                *pos += 1;
                addresses.push(parse_cidr_or_name(tokens, pos)?);
            }
            Some(other) => {
                return Err(err(tokens, *pos, format!(
                    "unexpected {other} in vxlan block"
                )));
            }
            None => {
                return Err(err(tokens, *pos,
                    "unexpected end of input in vxlan block".into(),
                ));
            }
        }
    }

    Ok(ast::VxlanDef {
        name,
        vni,
        local,
        remote,
        port,
        addresses,
    })
}

// ─── Dummy ────────────────────────────────────────────────

fn parse_dummy_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::DummyDef> {
    let name = expect_ident(tokens, pos)?;
    let mut addresses = Vec::new();

    if eat(tokens, pos, &Token::LBrace) {
        loop {
            skip_newlines(tokens, pos);
            if eat(tokens, pos, &Token::RBrace) {
                break;
            }
            match at(tokens, *pos) {
                Some(Token::Address) => {
                    *pos += 1;
                    addresses.push(parse_cidr_or_name(tokens, pos)?);
                }
                Some(other) => {
                    return Err(err(tokens, *pos, format!(
                        "unexpected {other} in dummy block"
                    )));
                }
                None => {
                    return Err(err(tokens, *pos,
                        "unexpected end of input in dummy block".into(),
                    ));
                }
            }
        }
    }

    Ok(ast::DummyDef { name, addresses })
}

// ─── Run ──────────────────────────────────────────────────

fn parse_run_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::RunDef> {
    let background = eat(tokens, pos, &Token::Background);
    let cmd = parse_string_list(tokens, pos)?;
    Ok(ast::RunDef { cmd, background })
}

// ─── Link ─────────────────────────────────────────────────

fn parse_link(tokens: &[Spanned], pos: &mut usize) -> Result<ast::LinkDef> {
    expect(tokens, pos, &Token::Link)?;
    let (left_node, left_iface) = parse_endpoint(tokens, pos)?;
    expect(tokens, pos, &Token::DashDash)?;
    let (right_node, right_iface) = parse_endpoint(tokens, pos)?;

    let mut link = ast::LinkDef {
        left_node,
        left_iface,
        right_node,
        right_iface,
        left_addr: None,
        right_addr: None,
        mtu: None,
        impairment: None,
        left_impair: None,
        right_impair: None,
        rate: None,
    };

    if eat(tokens, pos, &Token::LBrace) {
        loop {
            skip_newlines(tokens, pos);
            if eat(tokens, pos, &Token::RBrace) {
                break;
            }

            match at(tokens, *pos) {
                // Address pair: CIDR -- CIDR (may start with CIDR, Interp, or Int for compound addresses)
                Some(Token::Cidr(_)) | Some(Token::Interp(_)) | Some(Token::Int(_)) => {
                    let left_addr = parse_cidr_or_name(tokens, pos)?;
                    expect(tokens, pos, &Token::DashDash)?;
                    let right_addr = parse_cidr_or_name(tokens, pos)?;
                    link.left_addr = Some(left_addr);
                    link.right_addr = Some(right_addr);
                }
                // MTU
                Some(Token::Mtu) => {
                    *pos += 1;
                    link.mtu = Some(expect_int(tokens, pos)? as u32);
                }
                // Directional impairment ->
                Some(Token::ArrowRight) => {
                    *pos += 1;
                    link.left_impair = Some(parse_impair_props(tokens, pos)?);
                }
                // Directional impairment <-
                Some(Token::ArrowLeft) => {
                    *pos += 1;
                    link.right_impair = Some(parse_impair_props(tokens, pos)?);
                }
                // Rate
                Some(Token::Rate) => {
                    *pos += 1;
                    link.rate = Some(parse_rate_props(tokens, pos)?);
                }
                // Symmetric impairment (delay, jitter, loss, corrupt, reorder)
                Some(Token::Delay) | Some(Token::Jitter) | Some(Token::Loss)
                | Some(Token::Corrupt) | Some(Token::Reorder) => {
                    link.impairment = Some(parse_impair_props(tokens, pos)?);
                }
                Some(other) => {
                    return Err(err(tokens, *pos, format!(
                        "unexpected {other} in link block"
                    )));
                }
                None => {
                    return Err(err(tokens, *pos, 
                        "unexpected end of input in link block".into(),
                    ));
                }
            }
        }
    }

    Ok(link)
}

// ─── Impairment Properties ────────────────────────────────

fn parse_impair_props(tokens: &[Spanned], pos: &mut usize) -> Result<ast::ImpairProps> {
    let mut props = ast::ImpairProps::default();

    loop {
        match at(tokens, *pos) {
            Some(Token::Delay) => {
                *pos += 1;
                props.delay = Some(parse_value(tokens, pos)?);
            }
            Some(Token::Jitter) => {
                *pos += 1;
                props.jitter = Some(parse_value(tokens, pos)?);
            }
            Some(Token::Loss) => {
                *pos += 1;
                props.loss = Some(parse_value(tokens, pos)?);
            }
            Some(Token::Rate) => {
                *pos += 1;
                props.rate = Some(parse_value(tokens, pos)?);
            }
            Some(Token::Corrupt) => {
                *pos += 1;
                props.corrupt = Some(parse_value(tokens, pos)?);
            }
            Some(Token::Reorder) => {
                *pos += 1;
                props.reorder = Some(parse_value(tokens, pos)?);
            }
            _ => break,
        }
    }

    Ok(props)
}

// ─── Rate Properties ──────────────────────────────────────

fn parse_rate_props(tokens: &[Spanned], pos: &mut usize) -> Result<ast::RateProps> {
    let mut props = ast::RateProps::default();

    loop {
        match at(tokens, *pos) {
            Some(Token::Egress) => {
                *pos += 1;
                props.egress = Some(parse_value(tokens, pos)?);
            }
            Some(Token::Ingress) => {
                *pos += 1;
                props.ingress = Some(parse_value(tokens, pos)?);
            }
            Some(Token::Burst) => {
                *pos += 1;
                props.burst = Some(parse_value(tokens, pos)?);
            }
            _ => break,
        }
    }

    Ok(props)
}

// ─── Network ──────────────────────────────────────────────

fn parse_network(tokens: &[Spanned], pos: &mut usize) -> Result<ast::NetworkDef> {
    expect(tokens, pos, &Token::Network)?;
    let name = expect_ident(tokens, pos)?;

    let mut net = ast::NetworkDef {
        name,
        members: Vec::new(),
        vlan_filtering: false,
        mtu: None,
        vlans: Vec::new(),
        ports: Vec::new(),
    };

    expect(tokens, pos, &Token::LBrace)?;
    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }

        match at(tokens, *pos) {
            Some(Token::Members) => {
                *pos += 1;
                net.members = parse_endpoint_list(tokens, pos)?;
            }
            Some(Token::VlanFiltering) => {
                *pos += 1;
                net.vlan_filtering = true;
            }
            Some(Token::Mtu) => {
                *pos += 1;
                net.mtu = Some(expect_int(tokens, pos)? as u32);
            }
            Some(Token::Vlan) => {
                *pos += 1;
                let id = expect_int(tokens, pos)? as u16;
                let vlan_name = match at(tokens, *pos) {
                    Some(Token::String(_)) => Some(expect_string(tokens, pos)?),
                    _ => None,
                };
                net.vlans.push(ast::VlanDef { id, name: vlan_name });
            }
            Some(Token::Port) => {
                *pos += 1;
                let endpoint = parse_name(tokens, pos)?;
                let port_def = parse_port_block(tokens, pos, endpoint)?;
                net.ports.push(port_def);
            }
            Some(other) => {
                return Err(err(tokens, *pos, format!(
                    "unexpected {other} in network block"
                )));
            }
            None => {
                return Err(err(tokens, *pos, 
                    "unexpected end of input in network block".into(),
                ));
            }
        }
    }

    Ok(net)
}

fn parse_port_block(
    tokens: &[Spanned],
    pos: &mut usize,
    endpoint: String,
) -> Result<ast::PortDef> {
    let mut port = ast::PortDef {
        endpoint,
        pvid: None,
        vlans: Vec::new(),
        tagged: false,
        untagged: false,
    };

    expect(tokens, pos, &Token::LBrace)?;
    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }

        match at(tokens, *pos) {
            Some(Token::Pvid) => {
                *pos += 1;
                port.pvid = Some(expect_int(tokens, pos)? as u16);
            }
            Some(Token::Vlans) => {
                *pos += 1;
                port.vlans = parse_int_list(tokens, pos)?;
            }
            Some(Token::Tagged) => {
                *pos += 1;
                port.tagged = true;
            }
            Some(Token::Untagged) => {
                *pos += 1;
                port.untagged = true;
            }
            Some(other) => {
                return Err(err(tokens, *pos, format!(
                    "unexpected {other} in port block"
                )));
            }
            None => {
                return Err(err(tokens, *pos, 
                    "unexpected end of input in port block".into(),
                ));
            }
        }
    }

    Ok(port)
}

// ─── Standalone Impair/Rate ───────────────────────────────

fn parse_impair_stmt(tokens: &[Spanned], pos: &mut usize) -> Result<ast::ImpairDef> {
    expect(tokens, pos, &Token::Impair)?;
    let (node, iface) = parse_endpoint(tokens, pos)?;
    let props = parse_impair_props(tokens, pos)?;
    Ok(ast::ImpairDef {
        node,
        iface,
        props,
    })
}

fn parse_rate_stmt(tokens: &[Spanned], pos: &mut usize) -> Result<ast::RateDef> {
    expect(tokens, pos, &Token::Rate)?;
    let (node, iface) = parse_endpoint(tokens, pos)?;
    let props = parse_rate_props(tokens, pos)?;
    Ok(ast::RateDef {
        node,
        iface,
        props,
    })
}

// ─── Let / For ────────────────────────────────────────────

fn parse_let(tokens: &[Spanned], pos: &mut usize) -> Result<ast::LetDef> {
    expect(tokens, pos, &Token::Let)?;
    let name = expect_ident(tokens, pos)?;
    expect(tokens, pos, &Token::Eq)?;
    let value = parse_value(tokens, pos)?;
    Ok(ast::LetDef { name, value })
}

fn parse_for(tokens: &[Spanned], pos: &mut usize) -> Result<ast::ForLoop> {
    expect(tokens, pos, &Token::For)?;
    let var = expect_ident(tokens, pos)?;
    expect(tokens, pos, &Token::In)?;
    let start = expect_int(tokens, pos)?;
    expect(tokens, pos, &Token::DotDot)?;
    let end = expect_int(tokens, pos)?;

    expect(tokens, pos, &Token::LBrace)?;
    let mut body = Vec::new();

    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }
        body.push(parse_statement(tokens, pos)?);
    }

    Ok(ast::ForLoop {
        var,
        start,
        end,
        body,
    })
}

// ─── List Helpers ─────────────────────────────────────────

fn parse_string_list(tokens: &[Spanned], pos: &mut usize) -> Result<Vec<String>> {
    expect(tokens, pos, &Token::LBracket)?;
    let mut items = Vec::new();

    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBracket) {
            break;
        }
        if !items.is_empty() {
            expect(tokens, pos, &Token::Comma)?;
            skip_newlines(tokens, pos);
            if eat(tokens, pos, &Token::RBracket) {
                break;
            }
        }
        items.push(expect_string(tokens, pos)?);
    }

    Ok(items)
}

fn parse_ident_list(tokens: &[Spanned], pos: &mut usize) -> Result<Vec<String>> {
    expect(tokens, pos, &Token::LBracket)?;
    let mut items = Vec::new();

    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBracket) {
            break;
        }
        if !items.is_empty() {
            expect(tokens, pos, &Token::Comma)?;
            skip_newlines(tokens, pos);
            if eat(tokens, pos, &Token::RBracket) {
                break;
            }
        }
        items.push(expect_ident(tokens, pos)?);
    }

    Ok(items)
}

fn parse_endpoint_list(tokens: &[Spanned], pos: &mut usize) -> Result<Vec<String>> {
    expect(tokens, pos, &Token::LBracket)?;
    let mut items = Vec::new();

    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBracket) {
            break;
        }
        if !items.is_empty() {
            expect(tokens, pos, &Token::Comma)?;
            skip_newlines(tokens, pos);
            if eat(tokens, pos, &Token::RBracket) {
                break;
            }
        }
        // Parse node:iface as a single string
        let (node, iface) = parse_endpoint(tokens, pos)?;
        items.push(format!("{node}:{iface}"));
    }

    Ok(items)
}

fn parse_int_list(tokens: &[Spanned], pos: &mut usize) -> Result<Vec<u16>> {
    expect(tokens, pos, &Token::LBracket)?;
    let mut items = Vec::new();

    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBracket) {
            break;
        }
        if !items.is_empty() {
            expect(tokens, pos, &Token::Comma)?;
            skip_newlines(tokens, pos);
            if eat(tokens, pos, &Token::RBracket) {
                break;
            }
        }
        items.push(expect_int(tokens, pos)? as u16);
    }

    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::nll::lexer;

    fn parse_nll(input: &str) -> ast::File {
        let tokens = lexer::lex(input).unwrap();
        parse_tokens(&tokens, input).unwrap()
    }

    #[test]
    fn test_parse_lab_bare() {
        let ast = parse_nll(r#"lab "simple""#);
        assert_eq!(ast.lab.name, "simple");
        assert!(ast.lab.description.is_none());
        assert!(ast.lab.prefix.is_none());
        assert!(ast.statements.is_empty());
    }

    #[test]
    fn test_parse_lab_with_block() {
        let ast = parse_nll(r#"lab "test" { description "A test lab"  prefix "t" }"#);
        assert_eq!(ast.lab.name, "test");
        assert_eq!(ast.lab.description.as_deref(), Some("A test lab"));
        assert_eq!(ast.lab.prefix.as_deref(), Some("t"));
    }

    #[test]
    fn test_parse_bare_node() {
        let ast = parse_nll(r#"lab "t"
node host"#);
        assert_eq!(ast.statements.len(), 1);
        match &ast.statements[0] {
            ast::Statement::Node(n) => {
                assert_eq!(n.name, "host");
                assert!(n.profile.is_none());
                assert!(n.props.is_empty());
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_node_with_profile() {
        let ast = parse_nll(r#"lab "t"
node r1 : router"#);
        match &ast.statements[0] {
            ast::Statement::Node(n) => {
                assert_eq!(n.name, "r1");
                assert_eq!(n.profile.as_deref(), Some("router"));
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_node_with_forward() {
        let ast = parse_nll(r#"lab "t"
node r1 { forward ipv4 }"#);
        match &ast.statements[0] {
            ast::Statement::Node(n) => {
                assert_eq!(n.props.len(), 1);
                assert!(matches!(n.props[0], ast::NodeProp::Forward(ast::IpVersion::Ipv4)));
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_node_with_route() {
        let ast = parse_nll(r#"lab "t"
node h1 { route default via 10.0.0.1 }"#);
        match &ast.statements[0] {
            ast::Statement::Node(n) => match &n.props[0] {
                ast::NodeProp::Route(r) => {
                    assert_eq!(r.destination, "default");
                    assert_eq!(r.via.as_deref(), Some("10.0.0.1"));
                }
                _ => panic!("expected Route"),
            },
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_bare_link() {
        let ast = parse_nll(r#"lab "t"
link r1:eth0 -- r2:eth0"#);
        match &ast.statements[0] {
            ast::Statement::Link(l) => {
                assert_eq!(l.left_node, "r1");
                assert_eq!(l.left_iface, "eth0");
                assert_eq!(l.right_node, "r2");
                assert_eq!(l.right_iface, "eth0");
                assert!(l.left_addr.is_none());
            }
            _ => panic!("expected Link"),
        }
    }

    #[test]
    fn test_parse_link_with_addresses() {
        let ast = parse_nll(
            r#"lab "t"
link r1:eth0 -- r2:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }"#,
        );
        match &ast.statements[0] {
            ast::Statement::Link(l) => {
                assert_eq!(l.left_addr.as_deref(), Some("10.0.0.1/24"));
                assert_eq!(l.right_addr.as_deref(), Some("10.0.0.2/24"));
            }
            _ => panic!("expected Link"),
        }
    }

    #[test]
    fn test_parse_link_with_impairment() {
        let ast = parse_nll(
            r#"lab "t"
link a:e0 -- b:e0 {
  10.0.0.1/24 -- 10.0.0.2/24
  delay 10ms jitter 2ms loss 0.1%
}"#,
        );
        match &ast.statements[0] {
            ast::Statement::Link(l) => {
                let imp = l.impairment.as_ref().unwrap();
                assert_eq!(imp.delay.as_deref(), Some("10ms"));
                assert_eq!(imp.jitter.as_deref(), Some("2ms"));
                assert_eq!(imp.loss.as_deref(), Some("0.1%"));
            }
            _ => panic!("expected Link"),
        }
    }

    #[test]
    fn test_parse_link_asymmetric() {
        let ast = parse_nll(
            r#"lab "t"
link a:e0 -- b:e0 {
  10.0.0.1/30 -- 10.0.0.2/30
  -> delay 500ms rate 10mbit
  <- delay 500ms rate 2mbit
}"#,
        );
        match &ast.statements[0] {
            ast::Statement::Link(l) => {
                let left = l.left_impair.as_ref().unwrap();
                assert_eq!(left.delay.as_deref(), Some("500ms"));
                assert_eq!(left.rate.as_deref(), Some("10mbit"));
                let right = l.right_impair.as_ref().unwrap();
                assert_eq!(right.rate.as_deref(), Some("2mbit"));
            }
            _ => panic!("expected Link"),
        }
    }

    #[test]
    fn test_parse_profile() {
        let ast = parse_nll(
            r#"lab "t"
profile router { forward ipv4 }"#,
        );
        match &ast.statements[0] {
            ast::Statement::Profile(p) => {
                assert_eq!(p.name, "router");
                assert_eq!(p.props.len(), 1);
            }
            _ => panic!("expected Profile"),
        }
    }

    #[test]
    fn test_parse_for_loop() {
        let ast = parse_nll(
            r#"lab "t"
for i in 1..4 {
  node r1
}"#,
        );
        match &ast.statements[0] {
            ast::Statement::For(f) => {
                assert_eq!(f.var, "i");
                assert_eq!(f.start, 1);
                assert_eq!(f.end, 4);
                assert_eq!(f.body.len(), 1);
            }
            _ => panic!("expected For"),
        }
    }

    #[test]
    fn test_parse_let() {
        let ast = parse_nll(
            r#"lab "t"
let delay = 30ms"#,
        );
        match &ast.statements[0] {
            ast::Statement::Let(l) => {
                assert_eq!(l.name, "delay");
                assert_eq!(l.value, "30ms");
            }
            _ => panic!("expected Let"),
        }
    }

    #[test]
    fn test_parse_firewall() {
        let ast = parse_nll(
            r#"lab "t"
node server {
  firewall policy drop {
    accept ct established,related
    accept tcp dport 80
  }
}"#,
        );
        match &ast.statements[0] {
            ast::Statement::Node(n) => match &n.props[0] {
                ast::NodeProp::Firewall(fw) => {
                    assert_eq!(fw.policy, "drop");
                    assert_eq!(fw.rules.len(), 2);
                    assert_eq!(fw.rules[0].action, "accept");
                    assert_eq!(fw.rules[0].match_expr, "ct state established,related");
                    assert_eq!(fw.rules[1].match_expr, "tcp dport 80");
                }
                _ => panic!("expected Firewall"),
            },
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_vrf() {
        let ast = parse_nll(
            r#"lab "t"
node pe {
  vrf red table 10 {
    interfaces [eth1, eth2]
    route default dev eth1
  }
}"#,
        );
        match &ast.statements[0] {
            ast::Statement::Node(n) => match &n.props[0] {
                ast::NodeProp::Vrf(v) => {
                    assert_eq!(v.name, "red");
                    assert_eq!(v.table, 10);
                    assert_eq!(v.interfaces, vec!["eth1", "eth2"]);
                    assert_eq!(v.routes.len(), 1);
                    assert_eq!(v.routes[0].dev.as_deref(), Some("eth1"));
                }
                _ => panic!("expected Vrf"),
            },
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_wireguard() {
        let ast = parse_nll(
            r#"lab "t"
node gw {
  wireguard wg0 {
    key auto
    listen 51820
    address 192.168.255.1/32
    peers [gw-b]
  }
}"#,
        );
        match &ast.statements[0] {
            ast::Statement::Node(n) => match &n.props[0] {
                ast::NodeProp::Wireguard(wg) => {
                    assert_eq!(wg.name, "wg0");
                    assert_eq!(wg.key.as_deref(), Some("auto"));
                    assert_eq!(wg.listen_port, Some(51820));
                    assert_eq!(wg.addresses, vec!["192.168.255.1/32"]);
                    assert_eq!(wg.peers, vec!["gw-b"]);
                }
                _ => panic!("expected Wireguard"),
            },
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_network() {
        let ast = parse_nll(
            r#"lab "t"
network fabric {
  members [switch:br0, host1:eth0]
  vlan-filtering
  mtu 9000
  vlan 100 "sales"
  port host1 { pvid 100  untagged }
}"#,
        );
        match &ast.statements[0] {
            ast::Statement::Network(n) => {
                assert_eq!(n.name, "fabric");
                assert_eq!(n.members, vec!["switch:br0", "host1:eth0"]);
                assert!(n.vlan_filtering);
                assert_eq!(n.mtu, Some(9000));
                assert_eq!(n.vlans.len(), 1);
                assert_eq!(n.vlans[0].id, 100);
                assert_eq!(n.vlans[0].name.as_deref(), Some("sales"));
                assert_eq!(n.ports.len(), 1);
                assert_eq!(n.ports[0].pvid, Some(100));
                assert!(n.ports[0].untagged);
            }
            _ => panic!("expected Network"),
        }
    }

    #[test]
    fn test_parse_simple_full() {
        let input = r#"lab "simple"

node router { forward ipv4 }
node host { route default via 10.0.0.1 }

link router:eth0 -- host:eth0 {
  10.0.0.1/24 -- 10.0.0.2/24
  delay 10ms jitter 2ms
}"#;
        let ast = parse_nll(input);
        assert_eq!(ast.lab.name, "simple");
        assert_eq!(ast.statements.len(), 3); // 2 nodes + 1 link
    }
}
