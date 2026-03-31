//! NLL parser — converts token stream into AST.

use super::ast;
use super::lexer::Spanned;
use crate::error::Result;

/// Parse a token stream into an NLL AST.
pub fn parse_tokens(tokens: &[Spanned], _source: &str) -> Result<ast::File> {
    let mut pos = 0;

    // Parse optional imports before the lab declaration
    let mut imports = Vec::new();
    loop {
        skip_newlines(tokens, &mut pos);
        if pos < tokens.len() && tokens[pos].token == Token::Import {
            imports.push(parse_import(tokens, &mut pos)?);
        } else {
            break;
        }
    }

    let lab = parse_lab_decl(tokens, &mut pos)?;
    let mut statements = Vec::new();

    while pos < tokens.len() {
        skip_newlines(tokens, &mut pos);
        if pos >= tokens.len() {
            break;
        }
        statements.push(parse_statement(tokens, &mut pos)?);
    }

    Ok(ast::File {
        imports,
        lab,
        statements,
    })
}

// ─── Import ──────────────────────────────────────────────

fn parse_import(tokens: &[Spanned], pos: &mut usize) -> Result<ast::ImportDef> {
    expect(tokens, pos, &Token::Import)?;
    let path = expect_string(tokens, pos)?;
    expect(tokens, pos, &Token::As)?;
    let alias = expect_ident(tokens, pos)?;

    // Optional parametric import: (key=value, ...)
    let params = if eat(tokens, pos, &Token::LParen) {
        let mut params = Vec::new();
        loop {
            skip_newlines(tokens, pos);
            if check(tokens, *pos, &Token::RParen) {
                *pos += 1;
                break;
            }
            if !params.is_empty() {
                eat(tokens, pos, &Token::Comma);
            }
            let key = expect_ident(tokens, pos)?;
            expect(tokens, pos, &Token::Eq)?;
            let value = parse_value(tokens, pos)?;
            params.push((key, value));
        }
        params
    } else {
        vec![]
    };

    Ok(ast::ImportDef {
        path,
        alias,
        params,
    })
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
        return Err(err(
            tokens,
            *pos,
            format!("unexpected end of input, expected {expected}"),
        ));
    }
    if &tokens[*pos].token != expected {
        return Err(err(
            tokens,
            *pos,
            format!("expected {expected}, found {}", tokens[*pos].token),
        ));
    }
    *pos += 1;
    Ok(())
}

/// Check if current token is a specific keyword (as ident) and consume it.
fn eat_kw(tokens: &[Spanned], pos: &mut usize, kw: &str) -> bool {
    if matches!(at(tokens, *pos), Some(Token::Ident(s)) if s == kw) {
        *pos += 1;
        true
    } else {
        false
    }
}

/// Expect a specific keyword (as ident) and consume it, or error.
fn expect_kw(tokens: &[Spanned], pos: &mut usize, kw: &str) -> Result<()> {
    if eat_kw(tokens, pos, kw) {
        Ok(())
    } else {
        Err(err(tokens, *pos, format!("expected '{kw}'")))
    }
}

/// Check if current token is a specific keyword (as ident) without consuming.
fn check_kw(tokens: &[Spanned], pos: usize, kw: &str) -> bool {
    matches!(at(tokens, pos), Some(Token::Ident(s)) if s == kw)
}

fn expect_ident(tokens: &[Spanned], pos: &mut usize) -> Result<String> {
    if *pos >= tokens.len() {
        return Err(err(
            tokens,
            *pos,
            "unexpected end of input, expected identifier".into(),
        ));
    }
    // Accept both Ident and keywords-as-identifiers (e.g. `let delay = ...`)
    if let Some(name) = token_as_ident(&tokens[*pos].token) {
        *pos += 1;
        Ok(name)
    } else {
        Err(err(
            tokens,
            *pos,
            format!("expected identifier, found {}", tokens[*pos].token),
        ))
    }
}

/// Extract an identifier string from a token, treating reserved keywords as
/// identifiers in contexts where they are used as names.
fn token_as_ident(token: &Token) -> Option<String> {
    match token {
        Token::Ident(s) => Some(s.clone()),
        // Reserved keywords that may appear as identifiers in some contexts
        Token::Import => Some("import".into()),
        Token::As => Some("as".into()),
        Token::Defaults => Some("defaults".into()),
        Token::Pool => Some("pool".into()),
        Token::Validate => Some("validate".into()),
        Token::Mesh => Some("mesh".into()),
        Token::Ring => Some("ring".into()),
        Token::Star => Some("star".into()),
        Token::Rate => Some("rate".into()),
        _ => None,
    }
}

fn expect_string(tokens: &[Spanned], pos: &mut usize) -> Result<String> {
    if *pos >= tokens.len() {
        return Err(err(
            tokens,
            *pos,
            "unexpected end of input, expected string".into(),
        ));
    }
    match &tokens[*pos].token {
        Token::String(s) => {
            let s = s.clone();
            *pos += 1;
            Ok(s)
        }
        _ => Err(err(
            tokens,
            *pos,
            format!("expected string, found {}", tokens[*pos].token),
        )),
    }
}

fn expect_int(tokens: &[Spanned], pos: &mut usize) -> Result<i64> {
    if *pos >= tokens.len() {
        return Err(err(
            tokens,
            *pos,
            "unexpected end of input, expected integer".into(),
        ));
    }
    match &tokens[*pos].token {
        Token::Int(s) => {
            let v = s
                .parse::<i64>()
                .map_err(|e| err(tokens, *pos, format!("invalid integer '{s}': {e}")))?;
            *pos += 1;
            Ok(v)
        }
        _ => Err(err(
            tokens,
            *pos,
            format!("expected integer, found {}", tokens[*pos].token),
        )),
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
    let mut prev_end: usize = 0;

    loop {
        if *pos >= tokens.len() {
            break;
        }
        // After the first token, only consume adjacent tokens (no whitespace gap).
        // This prevents `node web image "nginx"` from merging `web` and `image`.
        if started && tokens[*pos].span.start != prev_end {
            break;
        }
        match &tokens[*pos].token {
            _ if !started && token_as_ident(&tokens[*pos].token).is_some() => {
                name.push_str(&token_as_ident(&tokens[*pos].token).unwrap());
                prev_end = tokens[*pos].span.end;
                *pos += 1;
                started = true;
            }
            Token::Ident(s) => {
                name.push_str(s);
                prev_end = tokens[*pos].span.end;
                *pos += 1;
                started = true;
            }
            Token::Interp(s) => {
                name.push_str(s);
                prev_end = tokens[*pos].span.end;
                *pos += 1;
                started = true;
            }
            Token::Int(s) if started => {
                // Integers only allowed after an ident/interp (e.g. `spine1`)
                name.push_str(s);
                prev_end = tokens[*pos].span.end;
                *pos += 1;
            }
            Token::Dot if started => {
                // Dots allowed for import prefixes (e.g. `dc.r1`)
                name.push('.');
                prev_end = tokens[*pos].span.end;
                *pos += 1;
            }
            _ => break,
        }
    }

    if name.is_empty() {
        return Err(err(
            tokens,
            *pos,
            format!(
                "expected name at position {}",
                if start < tokens.len() {
                    format!("(found {})", tokens[start].token)
                } else {
                    "end of input".into()
                }
            ),
        ));
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
        return Err(err(
            tokens,
            *pos,
            "unexpected end of input, expected value".into(),
        ));
    }
    let val = match &tokens[*pos].token {
        Token::String(s) => s.clone(),
        Token::Ident(s) => s.clone(),
        Token::Int(s) => s.clone(),
        Token::Cidr(s) => s.clone(),
        Token::Ipv4Addr(s) => s.clone(),
        Token::Ipv6Cidr(s) => s.clone(),
        Token::Ipv6Addr(s) => s.clone(),
        Token::Duration(s) => s.clone(),
        Token::RateLit(s) => s.clone(),
        Token::Percent(s) => s.clone(),
        Token::Interp(s) => s.clone(),
        other => {
            return Err(err(tokens, *pos, format!("expected value, found {other}")));
        }
    };
    *pos += 1;
    Ok(val)
}

/// Parse a value that must be a duration (e.g., 10ms, 5s) or interpolation.
fn expect_duration_or_value(tokens: &[Spanned], pos: &mut usize) -> Result<String> {
    if *pos >= tokens.len() {
        return Err(err(
            tokens,
            *pos,
            "expected duration (e.g., 10ms, 5s)".into(),
        ));
    }
    match &tokens[*pos].token {
        Token::Duration(s) => {
            let s = s.clone();
            *pos += 1;
            Ok(s)
        }
        Token::Interp(s) => {
            let s = s.clone();
            *pos += 1;
            Ok(s)
        }
        // Allow plain values for backward compat (let variables etc.)
        Token::Ident(s) => {
            let s = s.clone();
            *pos += 1;
            Ok(s)
        }
        Token::String(s) => {
            let s = s.clone();
            *pos += 1;
            Ok(s)
        }
        other => Err(err(
            tokens,
            *pos,
            format!("expected duration (e.g., 10ms, 5s), found {other}"),
        )),
    }
}

/// Parse a value that must be a rate literal (e.g., 100mbit) or interpolation.
fn expect_rate_or_value(tokens: &[Spanned], pos: &mut usize) -> Result<String> {
    if *pos >= tokens.len() {
        return Err(err(
            tokens,
            *pos,
            "expected rate (e.g., 100mbit, 1gbit)".into(),
        ));
    }
    match &tokens[*pos].token {
        Token::RateLit(s) => {
            let s = s.clone();
            *pos += 1;
            Ok(s)
        }
        Token::Interp(s) => {
            let s = s.clone();
            *pos += 1;
            Ok(s)
        }
        Token::Ident(s) => {
            let s = s.clone();
            *pos += 1;
            Ok(s)
        }
        Token::String(s) => {
            let s = s.clone();
            *pos += 1;
            Ok(s)
        }
        other => Err(err(
            tokens,
            *pos,
            format!("expected rate (e.g., 100mbit, 1gbit), found {other}"),
        )),
    }
}

/// Parse a value that must be a percentage (e.g., 0.1%) or interpolation.
fn expect_percent_or_value(tokens: &[Spanned], pos: &mut usize) -> Result<String> {
    if *pos >= tokens.len() {
        return Err(err(
            tokens,
            *pos,
            "expected percentage (e.g., 0.1%, 5%)".into(),
        ));
    }
    match &tokens[*pos].token {
        Token::Percent(s) => {
            let s = s.clone();
            *pos += 1;
            Ok(s)
        }
        Token::Interp(s) => {
            let s = s.clone();
            *pos += 1;
            Ok(s)
        }
        Token::Ident(s) => {
            let s = s.clone();
            *pos += 1;
            Ok(s)
        }
        Token::String(s) => {
            let s = s.clone();
            *pos += 1;
            Ok(s)
        }
        other => Err(err(
            tokens,
            *pos,
            format!("expected percentage (e.g., 0.1%, 5%), found {other}"),
        )),
    }
}

// ─── Lab Declaration ──────────────────────────────────────

fn parse_lab_decl(tokens: &[Spanned], pos: &mut usize) -> Result<ast::LabDecl> {
    skip_newlines(tokens, pos);
    expect(tokens, pos, &Token::Lab)?;

    let name = expect_string(tokens, pos)?;
    let mut description = None;
    let mut prefix = None;
    let mut runtime = None;
    let mut version = None;
    let mut author = None;
    let mut tags = Vec::new();
    let mut mgmt = None;
    let mut dns = None;

    // Parse optional inline runtime before block
    if eat_kw(tokens, pos, "runtime") {
        runtime = Some(expect_string(tokens, pos)?);
    }

    if eat(tokens, pos, &Token::LBrace) {
        loop {
            skip_newlines(tokens, pos);
            if eat(tokens, pos, &Token::RBrace) {
                break;
            }
            if eat_kw(tokens, pos, "description") {
                description = Some(expect_string(tokens, pos)?);
            } else if eat_kw(tokens, pos, "prefix") {
                prefix = Some(expect_string(tokens, pos)?);
            } else if eat_kw(tokens, pos, "runtime") {
                runtime = Some(expect_string(tokens, pos)?);
            } else if eat_kw(tokens, pos, "version") {
                version = Some(expect_string(tokens, pos)?);
            } else if eat_kw(tokens, pos, "author") {
                author = Some(expect_string(tokens, pos)?);
            } else if eat_kw(tokens, pos, "tags") {
                tags = parse_ident_list(tokens, pos)?;
            } else if eat_kw(tokens, pos, "mgmt") {
                mgmt = Some(parse_cidr_or_name(tokens, pos)?);
            } else if eat_kw(tokens, pos, "dns") {
                dns = Some(expect_ident(tokens, pos)?);
            } else {
                match at(tokens, *pos) {
                    Some(other) => {
                        return Err(err(
                            tokens,
                            *pos,
                            format!("unexpected {other} in lab block"),
                        ));
                    }
                    None => {
                        return Err(err(
                            tokens,
                            *pos,
                            "unexpected end of input in lab block".into(),
                        ));
                    }
                }
            }
        }
    }

    Ok(ast::LabDecl {
        name,
        description,
        prefix,
        runtime,
        version,
        author,
        tags,
        mgmt,
        dns,
    })
}

// ─── Statements ───────────────────────────────────────────

fn parse_statement(tokens: &[Spanned], pos: &mut usize) -> Result<ast::Statement> {
    skip_newlines(tokens, pos);
    if *pos >= tokens.len() {
        return Err(err(
            tokens,
            *pos,
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
        Token::Defaults => parse_defaults(tokens, pos).map(ast::Statement::Defaults),
        Token::Pool => parse_pool(tokens, pos).map(ast::Statement::Pool),
        Token::Mesh | Token::Ring | Token::Star => {
            parse_pattern(tokens, pos).map(ast::Statement::Pattern)
        }
        Token::Validate => parse_validate(tokens, pos).map(ast::Statement::Validate),
        Token::Scenario => parse_scenario(tokens, pos).map(ast::Statement::Scenario),
        Token::Benchmark => parse_benchmark(tokens, pos).map(ast::Statement::Benchmark),
        Token::Param => parse_param(tokens, pos).map(ast::Statement::Param),
        Token::Let => parse_let(tokens, pos).map(ast::Statement::Let),
        Token::For => parse_for(tokens, pos).map(ast::Statement::For),
        Token::Ident(s) if s == "site" => parse_site(tokens, pos).map(ast::Statement::Site),
        other => Err(err(
            tokens,
            *pos,
            format!(
                "expected statement (profile, node, link, network, impair, rate, defaults, pool, validate, scenario, site, param, let, for), found {other}"
            ),
        )),
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

    let profiles = if eat(tokens, pos, &Token::Colon) {
        let mut profiles = vec![parse_name(tokens, pos)?];
        while eat(tokens, pos, &Token::Comma) {
            profiles.push(parse_name(tokens, pos)?);
        }
        profiles
    } else {
        vec![]
    };

    // Parse inline image/cmd before the block
    let mut image = None;
    let mut cmd = None;
    let mut env = Vec::new();
    let mut volumes = Vec::new();
    let mut cpu = None;
    let mut memory = None;
    let mut privileged = false;
    let mut cap_add = Vec::new();
    let mut cap_drop = Vec::new();
    let mut entrypoint = None;
    let mut hostname = None;
    let mut workdir = None;
    let mut labels = Vec::new();
    let mut pull = None;
    let mut container_exec = Vec::new();
    let mut healthcheck = None;
    let mut healthcheck_interval = None;
    let mut healthcheck_timeout = None;
    let mut startup_delay = None;
    let mut env_file = None;
    let mut configs = Vec::new();
    let mut overlay = None;
    let mut depends_on = Vec::new();
    if eat_kw(tokens, pos, "image") {
        image = Some(expect_string(tokens, pos)?);
        if eat_kw(tokens, pos, "cmd") {
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
            if eat_kw(tokens, pos, "image") {
                image = Some(expect_string(tokens, pos)?);
            } else if eat_kw(tokens, pos, "cmd") {
                if check(tokens, *pos, &Token::LBracket) {
                    cmd = Some(parse_string_list(tokens, pos)?);
                } else {
                    cmd = Some(vec![expect_string(tokens, pos)?]);
                }
            } else if eat_kw(tokens, pos, "env") {
                env = parse_string_list(tokens, pos)?;
            } else if eat_kw(tokens, pos, "volumes") {
                volumes = parse_string_list(tokens, pos)?;
            } else if eat_kw(tokens, pos, "cpu") {
                cpu = Some(parse_value(tokens, pos)?);
            } else if eat_kw(tokens, pos, "memory") {
                memory = Some(parse_value(tokens, pos)?);
            } else if eat_kw(tokens, pos, "privileged") {
                privileged = true;
            } else if eat_kw(tokens, pos, "cap-add") {
                cap_add = parse_ident_list(tokens, pos)?;
            } else if eat_kw(tokens, pos, "cap-drop") {
                cap_drop = parse_ident_list(tokens, pos)?;
            } else if eat_kw(tokens, pos, "entrypoint") {
                entrypoint = Some(expect_string(tokens, pos)?);
            } else if eat_kw(tokens, pos, "hostname") {
                hostname = Some(expect_string(tokens, pos)?);
            } else if eat_kw(tokens, pos, "workdir") {
                workdir = Some(expect_string(tokens, pos)?);
            } else if eat_kw(tokens, pos, "labels") {
                labels = parse_string_list(tokens, pos)?;
            } else if eat_kw(tokens, pos, "pull") {
                pull = Some(parse_value(tokens, pos)?);
            } else if eat_kw(tokens, pos, "exec") {
                container_exec.push(expect_string(tokens, pos)?);
            } else if eat_kw(tokens, pos, "healthcheck") {
                healthcheck = Some(expect_string(tokens, pos)?);
                // Optional inline interval/timeout
                if eat(tokens, pos, &Token::LBrace) {
                    loop {
                        skip_newlines(tokens, pos);
                        if eat(tokens, pos, &Token::RBrace) {
                            break;
                        }
                        if eat_kw(tokens, pos, "interval") {
                            healthcheck_interval = Some(parse_value(tokens, pos)?);
                        } else if eat_kw(tokens, pos, "timeout") {
                            healthcheck_timeout = Some(parse_value(tokens, pos)?);
                        } else if eat_kw(tokens, pos, "retries") {
                            // retries stored in timeout field for now
                            // (can be split later)
                            let _ = parse_value(tokens, pos)?;
                        } else {
                            // Skip unknown properties
                            let _ = parse_value(tokens, pos)?;
                        }
                    }
                }
            } else if eat_kw(tokens, pos, "startup-delay") {
                startup_delay = Some(parse_value(tokens, pos)?);
            } else if eat_kw(tokens, pos, "env-file") {
                env_file = Some(expect_string(tokens, pos)?);
            } else if eat_kw(tokens, pos, "config") {
                let host = expect_string(tokens, pos)?;
                let container = expect_string(tokens, pos)?;
                configs.push((host, container));
            } else if eat_kw(tokens, pos, "overlay") {
                overlay = Some(expect_string(tokens, pos)?);
            } else if eat_kw(tokens, pos, "depends-on") {
                depends_on = parse_ident_list(tokens, pos)?;
            } else if check_kw(tokens, *pos, "route") {
                *pos += 1;
                let routes = parse_route_defs(tokens, pos)?;
                for r in routes {
                    props.push(ast::NodeProp::Route(r));
                }
            } else {
                props.push(parse_node_prop(tokens, pos)?);
            }
        }
        props
    } else {
        Vec::new()
    };

    Ok(ast::NodeDef {
        name,
        profiles,
        image,
        cmd,
        env,
        volumes,
        cpu,
        memory,
        privileged,
        cap_add,
        cap_drop,
        entrypoint,
        hostname,
        workdir,
        labels,
        pull,
        container_exec,
        healthcheck,
        healthcheck_interval,
        healthcheck_timeout,
        startup_delay,
        env_file,
        configs,
        overlay,
        depends_on,
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
        if check_kw(tokens, *pos, "route") {
            *pos += 1;
            let routes = parse_route_defs(tokens, pos)?;
            for r in routes {
                props.push(ast::NodeProp::Route(r));
            }
        } else {
            props.push(parse_node_prop(tokens, pos)?);
        }
    }

    Ok(props)
}

fn parse_node_prop(tokens: &[Spanned], pos: &mut usize) -> Result<ast::NodeProp> {
    if check_kw(tokens, *pos, "forward") {
        *pos += 1;
        let version = if eat_kw(tokens, pos, "ipv4") {
            ast::IpVersion::Ipv4
        } else if eat_kw(tokens, pos, "ipv6") {
            ast::IpVersion::Ipv6
        } else {
            return Err(err(
                tokens,
                *pos,
                format!(
                    "expected 'ipv4' or 'ipv6' after 'forward', found {}",
                    at(tokens, *pos).map_or("end of input".to_string(), |t| t.to_string())
                ),
            ));
        };
        Ok(ast::NodeProp::Forward(version))
    } else if check_kw(tokens, *pos, "sysctl") {
        *pos += 1;
        let key = expect_string(tokens, pos)?;
        let value = expect_string(tokens, pos)?;
        Ok(ast::NodeProp::Sysctl(key, value))
    } else if check_kw(tokens, *pos, "lo") {
        *pos += 1;
        let addr = parse_cidr_or_name(tokens, pos)?;
        Ok(ast::NodeProp::Lo(addr))
    // Note: "route" is handled at the call site (supports list destinations)
    } else if check_kw(tokens, *pos, "firewall") {
        *pos += 1;
        parse_firewall_def(tokens, pos).map(ast::NodeProp::Firewall)
    } else if check_kw(tokens, *pos, "nat") {
        *pos += 1;
        parse_nat_def(tokens, pos).map(ast::NodeProp::Nat)
    } else if check_kw(tokens, *pos, "vrf") {
        *pos += 1;
        parse_vrf_def(tokens, pos).map(ast::NodeProp::Vrf)
    } else if check_kw(tokens, *pos, "wireguard") {
        *pos += 1;
        parse_wireguard_def(tokens, pos).map(ast::NodeProp::Wireguard)
    } else if check_kw(tokens, *pos, "vxlan") {
        *pos += 1;
        parse_vxlan_def(tokens, pos).map(ast::NodeProp::Vxlan)
    } else if check_kw(tokens, *pos, "dummy") {
        *pos += 1;
        parse_dummy_def(tokens, pos).map(ast::NodeProp::Dummy)
    } else if check_kw(tokens, *pos, "macvlan") {
        *pos += 1;
        parse_macvlan_def(tokens, pos).map(ast::NodeProp::Macvlan)
    } else if check_kw(tokens, *pos, "ipvlan") {
        *pos += 1;
        parse_ipvlan_def(tokens, pos).map(ast::NodeProp::Ipvlan)
    } else if check_kw(tokens, *pos, "wifi") {
        *pos += 1;
        parse_wifi_def(tokens, pos).map(ast::NodeProp::Wifi)
    } else if check_kw(tokens, *pos, "run") {
        *pos += 1;
        parse_run_def(tokens, pos).map(ast::NodeProp::Run)
    } else {
        match at(tokens, *pos) {
            Some(other) => Err(err(
                tokens,
                *pos,
                format!(
                    "expected node property (forward, sysctl, lo, route, firewall, vrf, wireguard, vxlan, dummy, macvlan, ipvlan, wifi, run), found {other}"
                ),
            )),
            None => Err(err(
                tokens,
                *pos,
                "unexpected end of input in node block".into(),
            )),
        }
    }
}

fn parse_cidr_or_name(tokens: &[Spanned], pos: &mut usize) -> Result<String> {
    if *pos >= tokens.len() {
        return Err(err(
            tokens,
            *pos,
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
            Token::Cidr(s) | Token::Ipv4Addr(s) | Token::Ipv6Cidr(s) | Token::Ipv6Addr(s) => {
                val.push_str(s);
                *pos += 1;
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
        return Err(err(
            tokens,
            *pos,
            format!("expected CIDR or address, found {}", tokens[*pos].token),
        ));
    }
    Ok(val)
}

// ─── Route ────────────────────────────────────────────────

/// Parse one or more route definitions. Supports list destinations:
/// `route [10.0.0.0/8, 10.1.0.0/8] via 10.2.2.2`
fn parse_route_defs(tokens: &[Spanned], pos: &mut usize) -> Result<Vec<ast::RouteDef>> {
    // Check for list: route [cidr, cidr, ...] via/dev/metric
    if eat(tokens, pos, &Token::LBracket) {
        let mut destinations = Vec::new();
        loop {
            destinations.push(parse_cidr_or_name(tokens, pos)?);
            if !eat(tokens, pos, &Token::Comma) {
                break;
            }
        }
        expect(tokens, pos, &Token::RBracket)?;

        // Parse shared route parameters
        let mut via = None;
        let mut dev = None;
        let mut metric = None;
        loop {
            if check_kw(tokens, *pos, "via") {
                *pos += 1;
                via = Some(parse_cidr_or_name(tokens, pos)?);
            } else if check_kw(tokens, *pos, "dev") {
                *pos += 1;
                dev = Some(parse_name(tokens, pos)?);
            } else if check_kw(tokens, *pos, "metric") {
                *pos += 1;
                metric = Some(expect_int(tokens, pos)? as u32);
            } else {
                break;
            }
        }

        Ok(destinations
            .into_iter()
            .map(|dest| ast::RouteDef {
                destination: dest,
                via: via.clone(),
                dev: dev.clone(),
                metric,
            })
            .collect())
    } else {
        // Single destination
        parse_route_def(tokens, pos).map(|r| vec![r])
    }
}

fn parse_route_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::RouteDef> {
    // destination: "default" or CIDR
    let destination = if eat_kw(tokens, pos, "default") {
        "default".to_string()
    } else {
        parse_cidr_or_name(tokens, pos)?
    };

    let mut via = None;
    let mut dev = None;
    let mut metric = None;

    // Parse optional route parameters on same line
    loop {
        if check_kw(tokens, *pos, "via") {
            *pos += 1;
            via = Some(parse_cidr_or_name(tokens, pos)?);
        } else if check_kw(tokens, *pos, "dev") {
            *pos += 1;
            dev = Some(parse_name(tokens, pos)?);
        } else if check_kw(tokens, *pos, "metric") {
            *pos += 1;
            metric = Some(expect_int(tokens, pos)? as u32);
        } else {
            break;
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

// ─── NAT ──────────────────────────────────────────────────

// nat { masquerade src CIDR; dnat dst CIDR to IP; snat src CIDR to IP }
fn parse_nat_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::NatDef> {
    expect(tokens, pos, &Token::LBrace)?;
    let mut rules = Vec::new();

    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }

        if eat_kw(tokens, pos, "masquerade") {
            let src = if eat_kw(tokens, pos, "src") {
                Some(parse_cidr_or_name(tokens, pos)?)
            } else {
                None
            };
            rules.push(ast::NatRuleDef {
                action: "masquerade".into(),
                src,
                dst: None,
                target: None,
                target_port: None,
            });
        } else if eat_kw(tokens, pos, "dnat") {
            let dst = if eat_kw(tokens, pos, "dst") {
                Some(parse_cidr_or_name(tokens, pos)?)
            } else {
                None
            };
            expect_kw(tokens, pos, "to")?;
            let target = parse_value(tokens, pos)?;
            // Optional :port
            let target_port = if eat(tokens, pos, &Token::Colon) {
                Some(expect_int(tokens, pos)? as u16)
            } else {
                None
            };
            rules.push(ast::NatRuleDef {
                action: "dnat".into(),
                src: None,
                dst,
                target: Some(target),
                target_port,
            });
        } else if eat_kw(tokens, pos, "snat") {
            let src = if eat_kw(tokens, pos, "src") {
                Some(parse_cidr_or_name(tokens, pos)?)
            } else {
                None
            };
            expect_kw(tokens, pos, "to")?;
            let target = parse_value(tokens, pos)?;
            rules.push(ast::NatRuleDef {
                action: "snat".into(),
                src,
                dst: None,
                target: Some(target),
                target_port: None,
            });
        } else {
            match at(tokens, *pos) {
                Some(other) => {
                    return Err(err(
                        tokens,
                        *pos,
                        format!("expected NAT rule (masquerade, dnat, snat), found {other}"),
                    ));
                }
                None => {
                    return Err(err(
                        tokens,
                        *pos,
                        "unexpected end of input in nat block".into(),
                    ));
                }
            }
        }
    }

    Ok(ast::NatDef { rules })
}

fn parse_firewall_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::FirewallDef> {
    expect_kw(tokens, pos, "policy")?;
    let policy = if eat_kw(tokens, pos, "accept") {
        "accept".to_string()
    } else if eat_kw(tokens, pos, "drop") {
        "drop".to_string()
    } else if eat_kw(tokens, pos, "reject") {
        "reject".to_string()
    } else {
        match at(tokens, *pos) {
            Some(Token::Ident(s)) => {
                let s = s.clone();
                *pos += 1;
                s
            }
            other => {
                return Err(err(
                    tokens,
                    *pos,
                    format!(
                        "expected firewall policy, found {}",
                        other.map_or("end of input".to_string(), |t| t.to_string())
                    ),
                ));
            }
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
    let action = if eat_kw(tokens, pos, "accept") {
        "accept".to_string()
    } else if eat_kw(tokens, pos, "drop") {
        "drop".to_string()
    } else if eat_kw(tokens, pos, "reject") {
        "reject".to_string()
    } else {
        return Err(err(
            tokens,
            *pos,
            format!(
                "expected firewall action (accept/drop/reject), found {}",
                at(tokens, *pos).map_or("end of input".to_string(), |t| t.to_string())
            ),
        ));
    };

    // Parse match expression
    let match_expr = parse_match_expr(tokens, pos)?;

    Ok(ast::FirewallRuleDef { action, match_expr })
}

fn parse_match_expr(tokens: &[Spanned], pos: &mut usize) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut matched = false;

    // Parse match components — src/dst can appear before or after protocol matches
    loop {
        if check_kw(tokens, *pos, "src") {
            *pos += 1;
            let addr = parse_cidr_or_name(tokens, pos)?;
            let family = if addr.contains(':') { "ip6" } else { "ip" };
            parts.insert(0, format!("{family} saddr {addr}")); // saddr first in nftables order
            matched = true;
        } else if check_kw(tokens, *pos, "dst") {
            *pos += 1;
            let addr = parse_cidr_or_name(tokens, pos)?;
            let family = if addr.contains(':') { "ip6" } else { "ip" };
            // Insert after saddr if present, otherwise at start
            let insert_pos = parts
                .iter()
                .position(|p| !p.contains("saddr"))
                .unwrap_or(parts.len());
            parts.insert(insert_pos, format!("{family} daddr {addr}"));
            matched = true;
        } else if check_kw(tokens, *pos, "ct") {
            *pos += 1;
            let mut ct = "ct state ".to_string();
            let state = parse_name(tokens, pos)?;
            ct.push_str(&state);
            while eat(tokens, pos, &Token::Comma) {
                let state = parse_name(tokens, pos)?;
                ct.push(',');
                ct.push_str(&state);
            }
            parts.push(ct);
            matched = true;
        } else if check_kw(tokens, *pos, "tcp") {
            *pos += 1;
            let dir = if eat_kw(tokens, pos, "dport") {
                "dport"
            } else if eat_kw(tokens, pos, "sport") {
                "sport"
            } else {
                return Err(err(
                    tokens,
                    *pos,
                    format!(
                        "expected 'dport' or 'sport' after 'tcp', found {}",
                        at(tokens, *pos).map_or("end of input".to_string(), |t| t.to_string())
                    ),
                ));
            };
            let port = expect_int(tokens, pos)?;
            parts.push(format!("tcp {dir} {port}"));
            matched = true;
        } else if check_kw(tokens, *pos, "udp") {
            *pos += 1;
            let dir = if eat_kw(tokens, pos, "dport") {
                "dport"
            } else if eat_kw(tokens, pos, "sport") {
                "sport"
            } else {
                return Err(err(
                    tokens,
                    *pos,
                    format!(
                        "expected 'dport' or 'sport' after 'udp', found {}",
                        at(tokens, *pos).map_or("end of input".to_string(), |t| t.to_string())
                    ),
                ));
            };
            let port = expect_int(tokens, pos)?;
            parts.push(format!("udp {dir} {port}"));
            matched = true;
        } else if check_kw(tokens, *pos, "icmp") {
            *pos += 1;
            let icmp_type = expect_int(tokens, pos)?;
            parts.push(format!("icmp type {icmp_type}"));
            matched = true;
        } else if check_kw(tokens, *pos, "icmpv6") {
            *pos += 1;
            let icmp_type = expect_int(tokens, pos)?;
            parts.push(format!("icmpv6 type {icmp_type}"));
            matched = true;
        } else if check_kw(tokens, *pos, "mark") {
            *pos += 1;
            let mark = expect_int(tokens, pos)?;
            parts.push(format!("mark {mark}"));
            matched = true;
        } else {
            break;
        }
    }

    if !matched {
        let tok = at(tokens, *pos);
        return Err(err(
            tokens,
            *pos,
            format!(
                "expected match expression (ct/tcp/udp/icmp/icmpv6/mark/src/dst), found {}",
                tok.map_or("end of input".to_string(), |t| t.to_string())
            ),
        ));
    }

    Ok(parts.join(" "))
}

// ─── VRF ──────────────────────────────────────────────────

fn parse_vrf_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::VrfDef> {
    let name = expect_ident(tokens, pos)?;
    expect_kw(tokens, pos, "table")?;
    let table = expect_int(tokens, pos)? as u32;

    let mut interfaces = Vec::new();
    let mut routes = Vec::new();

    expect(tokens, pos, &Token::LBrace)?;
    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }
        if eat_kw(tokens, pos, "interfaces") {
            interfaces = parse_ident_list(tokens, pos)?;
        } else if eat_kw(tokens, pos, "route") {
            routes.push(parse_route_def(tokens, pos)?);
        } else {
            match at(tokens, *pos) {
                Some(other) => {
                    return Err(err(
                        tokens,
                        *pos,
                        format!("unexpected {other} in VRF block"),
                    ));
                }
                None => {
                    return Err(err(
                        tokens,
                        *pos,
                        "unexpected end of input in VRF block".into(),
                    ));
                }
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
        if eat_kw(tokens, pos, "key") {
            key = Some(parse_value(tokens, pos)?);
        } else if eat_kw(tokens, pos, "listen") {
            listen_port = Some(expect_int(tokens, pos)? as u16);
        } else if eat_kw(tokens, pos, "address") {
            addresses.push(parse_cidr_or_name(tokens, pos)?);
        } else if eat_kw(tokens, pos, "peers") {
            peers = parse_ident_list(tokens, pos)?;
        } else {
            match at(tokens, *pos) {
                Some(other) => {
                    return Err(err(
                        tokens,
                        *pos,
                        format!("unexpected {other} in wireguard block"),
                    ));
                }
                None => {
                    return Err(err(
                        tokens,
                        *pos,
                        "unexpected end of input in wireguard block".into(),
                    ));
                }
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
        if eat_kw(tokens, pos, "vni") {
            vni = expect_int(tokens, pos)? as u32;
        } else if eat_kw(tokens, pos, "local") {
            local = Some(parse_cidr_or_name(tokens, pos)?);
        } else if eat_kw(tokens, pos, "remote") {
            remote = Some(parse_cidr_or_name(tokens, pos)?);
        } else if eat_kw(tokens, pos, "port") {
            port = Some(expect_int(tokens, pos)? as u16);
        } else if eat_kw(tokens, pos, "address") {
            addresses.push(parse_cidr_or_name(tokens, pos)?);
        } else {
            match at(tokens, *pos) {
                Some(other) => {
                    return Err(err(
                        tokens,
                        *pos,
                        format!("unexpected {other} in vxlan block"),
                    ));
                }
                None => {
                    return Err(err(
                        tokens,
                        *pos,
                        "unexpected end of input in vxlan block".into(),
                    ));
                }
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
            if eat_kw(tokens, pos, "address") {
                addresses.push(parse_cidr_or_name(tokens, pos)?);
            } else {
                match at(tokens, *pos) {
                    Some(other) => {
                        return Err(err(
                            tokens,
                            *pos,
                            format!("unexpected {other} in dummy block"),
                        ));
                    }
                    None => {
                        return Err(err(
                            tokens,
                            *pos,
                            "unexpected end of input in dummy block".into(),
                        ));
                    }
                }
            }
        }
    }

    Ok(ast::DummyDef { name, addresses })
}

// ─── Macvlan ─────────────────────────────────────────────

// macvlan IDENT parent STRING (mode IDENT)? block?
fn parse_macvlan_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::MacvlanDef> {
    let name = expect_ident(tokens, pos)?;
    expect_kw(tokens, pos, "parent")?;
    let parent = parse_value(tokens, pos)?;
    let mut mode = None;
    let mut addresses = Vec::new();

    // Inline mode before block
    if check_kw(tokens, *pos, "mode") {
        *pos += 1;
        mode = Some(expect_ident(tokens, pos)?);
    }

    if eat(tokens, pos, &Token::LBrace) {
        loop {
            skip_newlines(tokens, pos);
            if eat(tokens, pos, &Token::RBrace) {
                break;
            }
            if check_kw(tokens, *pos, "mode") {
                *pos += 1;
                mode = Some(expect_ident(tokens, pos)?);
            } else if eat_kw(tokens, pos, "address") {
                addresses.push(parse_cidr_or_name(tokens, pos)?);
            } else if let Some(Token::Cidr(c)) = at(tokens, *pos) {
                addresses.push(c.clone());
                *pos += 1;
            } else {
                match at(tokens, *pos) {
                    Some(other) => {
                        return Err(err(
                            tokens,
                            *pos,
                            format!("unexpected {other} in macvlan block"),
                        ));
                    }
                    None => {
                        return Err(err(
                            tokens,
                            *pos,
                            "unexpected end of input in macvlan block".into(),
                        ));
                    }
                }
            }
        }
    }

    Ok(ast::MacvlanDef {
        name,
        parent,
        mode,
        addresses,
    })
}

// ─── Ipvlan ──────────────────────────────────────────────

// ipvlan IDENT parent STRING (mode IDENT)? block?
fn parse_ipvlan_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::IpvlanDef> {
    let name = expect_ident(tokens, pos)?;
    expect_kw(tokens, pos, "parent")?;
    let parent = parse_value(tokens, pos)?;
    let mut mode = None;
    let mut addresses = Vec::new();

    if check_kw(tokens, *pos, "mode") {
        *pos += 1;
        mode = Some(expect_ident(tokens, pos)?);
    }

    if eat(tokens, pos, &Token::LBrace) {
        loop {
            skip_newlines(tokens, pos);
            if eat(tokens, pos, &Token::RBrace) {
                break;
            }
            if check_kw(tokens, *pos, "mode") {
                *pos += 1;
                mode = Some(expect_ident(tokens, pos)?);
            } else if eat_kw(tokens, pos, "address") {
                addresses.push(parse_cidr_or_name(tokens, pos)?);
            } else if let Some(Token::Cidr(c)) = at(tokens, *pos) {
                addresses.push(c.clone());
                *pos += 1;
            } else {
                match at(tokens, *pos) {
                    Some(other) => {
                        return Err(err(
                            tokens,
                            *pos,
                            format!("unexpected {other} in ipvlan block"),
                        ));
                    }
                    None => {
                        return Err(err(
                            tokens,
                            *pos,
                            "unexpected end of input in ipvlan block".into(),
                        ));
                    }
                }
            }
        }
    }

    Ok(ast::IpvlanDef {
        name,
        parent,
        mode,
        addresses,
    })
}

// ─── Wifi ─────────────────────────────────────────────────

// wifi IDENT mode (ap|station|mesh) block?
fn parse_wifi_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::WifiDef> {
    let name = expect_ident(tokens, pos)?;

    // Expect "mode" as a context-sensitive ident
    if !check_kw(tokens, *pos, "mode") {
        return Err(err(
            tokens,
            *pos,
            "expected 'mode' after wifi interface name".into(),
        ));
    }
    *pos += 1;

    let mode = expect_ident(tokens, pos)?;
    if !matches!(mode.as_str(), "ap" | "station" | "mesh") {
        return Err(err(
            tokens,
            *pos - 1,
            format!("invalid wifi mode '{mode}': expected ap, station, or mesh"),
        ));
    }

    let mut ssid = None;
    let mut channel = None;
    let mut passphrase = None;
    let mut mesh_id = None;
    let mut addresses = Vec::new();

    if eat(tokens, pos, &Token::LBrace) {
        loop {
            skip_newlines(tokens, pos);
            if eat(tokens, pos, &Token::RBrace) {
                break;
            }
            if eat_kw(tokens, pos, "ssid") {
                ssid = Some(expect_string(tokens, pos)?);
            } else if check_kw(tokens, *pos, "channel") {
                *pos += 1;
                channel = Some(expect_int(tokens, pos)? as u32);
            } else if eat_kw(tokens, pos, "wpa2") {
                passphrase = Some(expect_string(tokens, pos)?);
            } else if eat_kw(tokens, pos, "mesh-id") {
                mesh_id = Some(expect_string(tokens, pos)?);
            } else if let Some(Token::Cidr(c)) = at(tokens, *pos) {
                addresses.push(c.clone());
                *pos += 1;
            } else if eat_kw(tokens, pos, "address") {
                addresses.push(parse_cidr_or_name(tokens, pos)?);
            } else {
                match at(tokens, *pos) {
                    Some(other) => {
                        return Err(err(
                            tokens,
                            *pos,
                            format!("unexpected {other} in wifi block"),
                        ));
                    }
                    None => {
                        return Err(err(
                            tokens,
                            *pos,
                            "unexpected end of input in wifi block".into(),
                        ));
                    }
                }
            }
        }
    }

    Ok(ast::WifiDef {
        name,
        mode,
        ssid,
        channel,
        passphrase,
        mesh_id,
        addresses,
    })
}

// ─── Run ──────────────────────────────────────────────────

fn parse_run_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::RunDef> {
    let background = eat_kw(tokens, pos, "background");
    // Shell-style: run "cmd arg1 arg2" → ["sh", "-c", "cmd arg1 arg2"]
    // List-style:  run ["cmd", "arg1"] → ["cmd", "arg1"]
    let cmd = if matches!(at(tokens, *pos), Some(Token::String(_))) {
        let shell_cmd = expect_string(tokens, pos)?;
        vec!["sh".to_string(), "-c".to_string(), shell_cmd]
    } else {
        parse_string_list(tokens, pos)?
    };
    Ok(ast::RunDef { cmd, background })
}

// ─── Link ─────────────────────────────────────────────────

fn parse_link(tokens: &[Spanned], pos: &mut usize) -> Result<ast::LinkDef> {
    expect(tokens, pos, &Token::Link)?;
    let (left_node, left_iface) = parse_endpoint(tokens, pos)?;
    expect(tokens, pos, &Token::DashDash)?;
    let (right_node, right_iface) = parse_endpoint(tokens, pos)?;

    // Optional `: profile` after endpoints
    let profile = if eat(tokens, pos, &Token::Colon) {
        Some(expect_ident(tokens, pos)?)
    } else {
        None
    };

    let mut link = ast::LinkDef {
        left_node,
        left_iface,
        right_node,
        right_iface,
        left_addr: None,
        right_addr: None,
        subnet: None,
        pool: None,
        mtu: None,
        impairment: None,
        left_impair: None,
        right_impair: None,
        rate: None,
        profile,
    };

    if eat(tokens, pos, &Token::LBrace) {
        loop {
            skip_newlines(tokens, pos);
            if eat(tokens, pos, &Token::RBrace) {
                break;
            }

            match at(tokens, *pos) {
                // Address pair: CIDR -- CIDR (IPv4 or IPv6)
                Some(Token::Cidr(_))
                | Some(Token::Ipv6Cidr(_))
                | Some(Token::Ipv6Addr(_))
                | Some(Token::Interp(_))
                | Some(Token::Int(_)) => {
                    let first_addr = parse_cidr_or_name(tokens, pos)?;
                    expect(tokens, pos, &Token::DashDash)?;
                    let right_addr = parse_cidr_or_name(tokens, pos)?;
                    link.left_addr = Some(first_addr);
                    link.right_addr = Some(right_addr);
                }
                // Subnet auto-assignment: subnet 10.0.0.0/30
                Some(Token::Ident(s)) if s == "subnet" => {
                    *pos += 1;
                    link.subnet = Some(parse_cidr_or_name(tokens, pos)?);
                }
                // MTU
                Some(Token::Ident(s)) if s == "mtu" => {
                    *pos += 1;
                    link.mtu = Some(expect_int(tokens, pos)? as u32);
                }
                // Pool reference
                Some(Token::Pool) => {
                    *pos += 1;
                    link.pool = Some(expect_ident(tokens, pos)?);
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
                Some(Token::Ident(s))
                    if matches!(
                        s.as_str(),
                        "delay" | "jitter" | "loss" | "corrupt" | "reorder"
                    ) =>
                {
                    link.impairment = Some(parse_impair_props(tokens, pos)?);
                }
                Some(other) => {
                    return Err(err(
                        tokens,
                        *pos,
                        format!("unexpected {other} in link block"),
                    ));
                }
                None => {
                    return Err(err(
                        tokens,
                        *pos,
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
        if check_kw(tokens, *pos, "delay") {
            *pos += 1;
            props.delay = Some(expect_duration_or_value(tokens, pos)?);
        } else if check_kw(tokens, *pos, "jitter") {
            *pos += 1;
            props.jitter = Some(expect_duration_or_value(tokens, pos)?);
        } else if check_kw(tokens, *pos, "loss") {
            *pos += 1;
            props.loss = Some(expect_percent_or_value(tokens, pos)?);
        } else if check(tokens, *pos, &Token::Rate) {
            *pos += 1;
            props.rate = Some(expect_rate_or_value(tokens, pos)?);
        } else if check_kw(tokens, *pos, "corrupt") {
            *pos += 1;
            props.corrupt = Some(expect_percent_or_value(tokens, pos)?);
        } else if check_kw(tokens, *pos, "reorder") {
            *pos += 1;
            props.reorder = Some(expect_percent_or_value(tokens, pos)?);
        } else {
            break;
        }
    }

    Ok(props)
}

// ─── Rate Properties ──────────────────────────────────────

fn parse_rate_props(tokens: &[Spanned], pos: &mut usize) -> Result<ast::RateProps> {
    let mut props = ast::RateProps::default();

    loop {
        if check_kw(tokens, *pos, "egress") {
            *pos += 1;
            props.egress = Some(parse_value(tokens, pos)?);
        } else if check_kw(tokens, *pos, "ingress") {
            *pos += 1;
            props.ingress = Some(parse_value(tokens, pos)?);
        } else if check_kw(tokens, *pos, "burst") {
            *pos += 1;
            props.burst = Some(parse_value(tokens, pos)?);
        } else {
            break;
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
        subnet: None,
        vlans: Vec::new(),
        ports: Vec::new(),
    };

    expect(tokens, pos, &Token::LBrace)?;
    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }

        if eat_kw(tokens, pos, "members") {
            net.members = parse_endpoint_list(tokens, pos)?;
        } else if eat_kw(tokens, pos, "vlan-filtering") {
            net.vlan_filtering = true;
        } else if eat_kw(tokens, pos, "subnet") {
            net.subnet = Some(parse_cidr_or_name(tokens, pos)?);
        } else if eat_kw(tokens, pos, "mtu") {
            net.mtu = Some(expect_int(tokens, pos)? as u32);
        } else if eat_kw(tokens, pos, "vlan") {
            let id = expect_int(tokens, pos)? as u16;
            let vlan_name = match at(tokens, *pos) {
                Some(Token::String(_)) => Some(expect_string(tokens, pos)?),
                _ => None,
            };
            net.vlans.push(ast::VlanDef {
                id,
                name: vlan_name,
            });
        } else if eat_kw(tokens, pos, "port") {
            let endpoint = parse_name(tokens, pos)?;
            let port_def = parse_port_block(tokens, pos, endpoint)?;
            net.ports.push(port_def);
        } else {
            match at(tokens, *pos) {
                Some(other) => {
                    return Err(err(
                        tokens,
                        *pos,
                        format!("unexpected {other} in network block"),
                    ));
                }
                None => {
                    return Err(err(
                        tokens,
                        *pos,
                        "unexpected end of input in network block".into(),
                    ));
                }
            }
        }
    }

    Ok(net)
}

fn parse_port_block(tokens: &[Spanned], pos: &mut usize, endpoint: String) -> Result<ast::PortDef> {
    let mut port = ast::PortDef {
        endpoint,
        pvid: None,
        vlans: Vec::new(),
        tagged: false,
        untagged: false,
        addresses: Vec::new(),
    };

    expect(tokens, pos, &Token::LBrace)?;
    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }

        if eat_kw(tokens, pos, "pvid") {
            port.pvid = Some(expect_int(tokens, pos)? as u16);
        } else if eat_kw(tokens, pos, "vlans") {
            port.vlans = parse_int_list(tokens, pos)?;
        } else if eat_kw(tokens, pos, "tagged") {
            port.tagged = true;
        } else if eat_kw(tokens, pos, "untagged") {
            port.untagged = true;
        } else if matches!(at(tokens, *pos), Some(Token::Cidr(_))) {
            if let Some(Token::Cidr(c)) = at(tokens, *pos) {
                port.addresses.push(c.clone());
                *pos += 1;
            }
        } else {
            match at(tokens, *pos) {
                Some(other) => {
                    return Err(err(
                        tokens,
                        *pos,
                        format!("unexpected {other} in port block"),
                    ));
                }
                None => {
                    return Err(err(
                        tokens,
                        *pos,
                        "unexpected end of input in port block".into(),
                    ));
                }
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
    Ok(ast::ImpairDef { node, iface, props })
}

fn parse_rate_stmt(tokens: &[Spanned], pos: &mut usize) -> Result<ast::RateDef> {
    expect(tokens, pos, &Token::Rate)?;
    let (node, iface) = parse_endpoint(tokens, pos)?;
    let props = parse_rate_props(tokens, pos)?;
    Ok(ast::RateDef { node, iface, props })
}

// ─── Defaults ─────────────────────────────────────────────

fn parse_defaults(tokens: &[Spanned], pos: &mut usize) -> Result<ast::DefaultsDef> {
    expect(tokens, pos, &Token::Defaults)?;

    let kind = match at(tokens, *pos) {
        Some(Token::Link) => {
            *pos += 1;
            ast::DefaultsKind::Link
        }
        Some(Token::Impair) => {
            *pos += 1;
            ast::DefaultsKind::Impair
        }
        Some(Token::Rate) => {
            *pos += 1;
            ast::DefaultsKind::Rate
        }
        Some(Token::Ident(name)) => {
            let name = name.clone();
            *pos += 1;
            ast::DefaultsKind::Named(name)
        }
        Some(other) => {
            return Err(err(
                tokens,
                *pos,
                format!(
                    "expected link, impair, rate, or profile name after defaults, found {other}"
                ),
            ));
        }
        None => {
            return Err(err(
                tokens,
                *pos,
                "unexpected end of input after defaults".into(),
            ));
        }
    };

    expect(tokens, pos, &Token::LBrace)?;

    let mut def = ast::DefaultsDef {
        kind: kind.clone(),
        mtu: None,
        impair: None,
        rate: None,
    };

    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }

        match (&kind, at(tokens, *pos)) {
            (ast::DefaultsKind::Link, Some(Token::Ident(s))) if s == "mtu" => {
                *pos += 1;
                def.mtu = Some(expect_int(tokens, pos)? as u32);
            }
            (ast::DefaultsKind::Impair, _) => {
                def.impair = Some(parse_impair_props(tokens, pos)?);
            }
            (ast::DefaultsKind::Rate, _) => {
                def.rate = Some(parse_rate_props(tokens, pos)?);
            }
            (ast::DefaultsKind::Named(_), Some(Token::Ident(s))) if s == "mtu" => {
                *pos += 1;
                def.mtu = Some(expect_int(tokens, pos)? as u32);
            }
            (ast::DefaultsKind::Named(_), _) => {
                // Named profiles accept impairment properties
                def.impair = Some(parse_impair_props(tokens, pos)?);
            }
            (_, Some(other)) => {
                return Err(err(
                    tokens,
                    *pos,
                    format!("unexpected {other} in defaults block"),
                ));
            }
            (_, None) => {
                return Err(err(
                    tokens,
                    *pos,
                    "unexpected end of input in defaults block".into(),
                ));
            }
        }
    }

    Ok(def)
}

// ─── Let / For ────────────────────────────────────────────

fn parse_pattern(tokens: &[Spanned], pos: &mut usize) -> Result<ast::PatternDef> {
    let kind_token = tokens[*pos].token.clone();
    *pos += 1;
    let name = expect_ident(tokens, pos)?;
    expect(tokens, pos, &Token::LBrace)?;

    let mut nodes = Vec::new();
    let mut count = None;
    let mut pool = None;
    let mut profile = None;
    let mut hub = None;
    let mut spokes = Vec::new();

    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }
        if check(tokens, *pos, &Token::Node) {
            // nodes [n1, n2, n3]
            *pos += 1;
            nodes = parse_ident_list(tokens, pos)?;
        } else if eat_kw(tokens, pos, "count") {
            count = Some(expect_int(tokens, pos)?);
        } else if check(tokens, *pos, &Token::Pool) {
            *pos += 1;
            pool = Some(expect_ident(tokens, pos)?);
        } else if check(tokens, *pos, &Token::Profile) {
            *pos += 1;
            profile = Some(expect_ident(tokens, pos)?);
        } else if eat_kw(tokens, pos, "hub") {
            hub = Some(expect_ident(tokens, pos)?);
        } else if eat_kw(tokens, pos, "spokes") {
            spokes = parse_ident_list(tokens, pos)?;
        } else {
            *pos += 1; // skip unknown
        }
    }

    let kind = match kind_token {
        Token::Mesh => ast::PatternKind::Mesh,
        Token::Ring => ast::PatternKind::Ring,
        Token::Star => {
            let hub_name = hub.unwrap_or_else(|| "hub".to_string());
            nodes = spokes;
            ast::PatternKind::Star { hub: hub_name }
        }
        _ => unreachable!(),
    };

    Ok(ast::PatternDef {
        kind,
        name,
        nodes,
        count,
        pool,
        profile,
    })
}

fn parse_pool(tokens: &[Spanned], pos: &mut usize) -> Result<ast::PoolDef> {
    expect(tokens, pos, &Token::Pool)?;
    let name = expect_ident(tokens, pos)?;
    let base = parse_cidr_or_name(tokens, pos)?;
    // Parse allocation prefix: /30, /31, /24, etc.
    expect(tokens, pos, &Token::Slash)?;
    let prefix = expect_int(tokens, pos)? as u8;
    Ok(ast::PoolDef { name, base, prefix })
}

fn parse_validate(tokens: &[Spanned], pos: &mut usize) -> Result<ast::ValidateDef> {
    expect(tokens, pos, &Token::Validate)?;
    let assertions = parse_assertion_block(tokens, pos)?;
    Ok(ast::ValidateDef { assertions })
}

/// Parse `{ assertion* }` block — shared between validate and scenario validate actions.
fn parse_assertion_block(tokens: &[Spanned], pos: &mut usize) -> Result<Vec<ast::AssertionDef>> {
    expect(tokens, pos, &Token::LBrace)?;
    let mut assertions = Vec::new();
    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }
        if eat_kw(tokens, pos, "reach") {
            let from = expect_ident(tokens, pos)?;
            let to = expect_ident(tokens, pos)?;
            assertions.push(ast::AssertionDef::Reach { from, to });
        } else if eat_kw(tokens, pos, "no-reach") {
            let from = expect_ident(tokens, pos)?;
            let to = expect_ident(tokens, pos)?;
            assertions.push(ast::AssertionDef::NoReach { from, to });
        } else if eat_kw(tokens, pos, "tcp-connect") {
            let from = expect_ident(tokens, pos)?;
            let to = expect_ident(tokens, pos)?;
            let port = expect_int(tokens, pos)? as u16;
            let timeout = if eat_kw(tokens, pos, "timeout") {
                Some(expect_duration_or_value(tokens, pos)?)
            } else {
                None
            };
            assertions.push(ast::AssertionDef::TcpConnect {
                from,
                to,
                port,
                timeout,
            });
        } else if eat_kw(tokens, pos, "latency-under") {
            let from = expect_ident(tokens, pos)?;
            let to = expect_ident(tokens, pos)?;
            let max = expect_duration_or_value(tokens, pos)?;
            let samples = if eat_kw(tokens, pos, "samples") {
                Some(expect_int(tokens, pos)? as u32)
            } else {
                None
            };
            assertions.push(ast::AssertionDef::LatencyUnder {
                from,
                to,
                max,
                samples,
            });
        } else if eat_kw(tokens, pos, "route-has") {
            let node = expect_ident(tokens, pos)?;
            let destination = parse_value(tokens, pos)?;
            let mut via = None;
            let mut dev = None;
            while check_kw(tokens, *pos, "via") || check_kw(tokens, *pos, "dev") {
                if check_kw(tokens, *pos, "via") {
                    *pos += 1;
                    via = Some(parse_value(tokens, pos)?);
                } else if check_kw(tokens, *pos, "dev") {
                    *pos += 1;
                    dev = Some(expect_ident(tokens, pos)?);
                } else {
                    break;
                }
            }
            assertions.push(ast::AssertionDef::RouteHas {
                node,
                destination,
                via,
                dev,
            });
        } else if eat_kw(tokens, pos, "dns-resolves") {
            let from = expect_ident(tokens, pos)?;
            let name = parse_value(tokens, pos)?;
            let expected_ip = parse_value(tokens, pos)?;
            assertions.push(ast::AssertionDef::DnsResolves {
                from,
                name,
                expected_ip,
            });
        } else {
            match at(tokens, *pos) {
                Some(other) => {
                    return Err(err(
                        tokens,
                        *pos,
                        format!("expected assertion in validate block, found {other}"),
                    ));
                }
                None => {
                    return Err(err(
                        tokens,
                        *pos,
                        "unexpected end of input in validate block".into(),
                    ));
                }
            }
        }
    }
    Ok(assertions)
}

// ─── Scenario ────────────────────────────────────────────

fn parse_scenario(tokens: &[Spanned], pos: &mut usize) -> Result<ast::ScenarioDef> {
    expect(tokens, pos, &Token::Scenario)?;
    let name = expect_string(tokens, pos)?;
    expect(tokens, pos, &Token::LBrace)?;

    let mut steps = Vec::new();
    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }
        expect_kw(tokens, pos, "at")?;
        let time = expect_duration_or_value(tokens, pos)?;
        expect(tokens, pos, &Token::LBrace)?;

        let mut actions = Vec::new();
        loop {
            skip_newlines(tokens, pos);
            if eat(tokens, pos, &Token::RBrace) {
                break;
            }
            if eat_kw(tokens, pos, "down") {
                let (node, iface) = parse_endpoint(tokens, pos)?;
                actions.push(ast::ScenarioActionDef::Down(format!("{node}:{iface}")));
            } else if eat_kw(tokens, pos, "up") {
                let (node, iface) = parse_endpoint(tokens, pos)?;
                actions.push(ast::ScenarioActionDef::Up(format!("{node}:{iface}")));
            } else if eat_kw(tokens, pos, "clear") {
                let (node, iface) = parse_endpoint(tokens, pos)?;
                actions.push(ast::ScenarioActionDef::Clear(format!("{node}:{iface}")));
            } else if check(tokens, *pos, &Token::Validate) {
                *pos += 1;
                let assertions = parse_assertion_block(tokens, pos)?;
                actions.push(ast::ScenarioActionDef::Validate(assertions));
            } else if eat_kw(tokens, pos, "exec") {
                let node = expect_ident(tokens, pos)?;
                let mut cmd = Vec::new();
                while matches!(at(tokens, *pos), Some(Token::String(_))) {
                    cmd.push(expect_string(tokens, pos)?);
                }
                actions.push(ast::ScenarioActionDef::Exec { node, cmd });
            } else if eat_kw(tokens, pos, "log") {
                let msg = expect_string(tokens, pos)?;
                actions.push(ast::ScenarioActionDef::Log(msg));
            } else {
                match at(tokens, *pos) {
                    Some(other) => {
                        return Err(err(
                            tokens,
                            *pos,
                            format!(
                                "expected scenario action (down, up, clear, validate, exec, log), found {other}"
                            ),
                        ));
                    }
                    None => {
                        return Err(err(
                            tokens,
                            *pos,
                            "unexpected end of input in scenario step".into(),
                        ));
                    }
                }
            }
        }

        steps.push(ast::ScenarioStepDef { time, actions });
    }

    Ok(ast::ScenarioDef { name, steps })
}

// ─── Benchmark ───────────────────────────────────────────

fn parse_benchmark(tokens: &[Spanned], pos: &mut usize) -> Result<ast::BenchmarkDef> {
    expect(tokens, pos, &Token::Benchmark)?;
    let name = expect_string(tokens, pos)?;
    expect(tokens, pos, &Token::LBrace)?;

    let mut tests = Vec::new();
    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }
        if check_kw(tokens, *pos, "iperf3") {
            *pos += 1;
            let from = expect_ident(tokens, pos)?;
            let to = expect_ident(tokens, pos)?;
            let mut duration = None;
            let mut streams = None;
            let mut udp = false;
            let mut assertions = Vec::new();

            if eat(tokens, pos, &Token::LBrace) {
                loop {
                    skip_newlines(tokens, pos);
                    if eat(tokens, pos, &Token::RBrace) {
                        break;
                    }
                    if eat_kw(tokens, pos, "duration") {
                        duration = Some(expect_duration_or_value(tokens, pos)?);
                    } else if eat_kw(tokens, pos, "streams") {
                        streams = Some(expect_int(tokens, pos)? as u32);
                    } else if eat_kw(tokens, pos, "udp") {
                        udp = true;
                    } else if eat_kw(tokens, pos, "assert") {
                        assertions.push(parse_benchmark_assertion(tokens, pos)?);
                    } else {
                        match at(tokens, *pos) {
                            Some(other) => {
                                return Err(err(
                                    tokens,
                                    *pos,
                                    format!("unexpected {other} in iperf3 block"),
                                ));
                            }
                            None => {
                                return Err(err(
                                    tokens,
                                    *pos,
                                    "unexpected end of input in iperf3 block".into(),
                                ));
                            }
                        }
                    }
                }
            }

            tests.push(ast::BenchmarkTestDef::Iperf3 {
                from,
                to,
                duration,
                streams,
                udp,
                assertions,
            });
        } else if check_kw(tokens, *pos, "ping") {
            *pos += 1;
            let from = expect_ident(tokens, pos)?;
            let to = expect_ident(tokens, pos)?;
            let mut count = None;
            let mut assertions = Vec::new();

            if eat(tokens, pos, &Token::LBrace) {
                loop {
                    skip_newlines(tokens, pos);
                    if eat(tokens, pos, &Token::RBrace) {
                        break;
                    }
                    if eat_kw(tokens, pos, "count") {
                        count = Some(expect_int(tokens, pos)? as u32);
                    } else if eat_kw(tokens, pos, "assert") {
                        assertions.push(parse_benchmark_assertion(tokens, pos)?);
                    } else {
                        match at(tokens, *pos) {
                            Some(other) => {
                                return Err(err(
                                    tokens,
                                    *pos,
                                    format!("unexpected {other} in ping block"),
                                ));
                            }
                            None => {
                                return Err(err(
                                    tokens,
                                    *pos,
                                    "unexpected end of input in ping block".into(),
                                ));
                            }
                        }
                    }
                }
            }

            tests.push(ast::BenchmarkTestDef::Ping {
                from,
                to,
                count,
                assertions,
            });
        } else {
            match at(tokens, *pos) {
                Some(other) => {
                    return Err(err(
                        tokens,
                        *pos,
                        format!("expected benchmark test (iperf3, ping), found {other}"),
                    ));
                }
                None => {
                    return Err(err(
                        tokens,
                        *pos,
                        "unexpected end of input in benchmark block".into(),
                    ));
                }
            }
        }
    }

    Ok(ast::BenchmarkDef { name, tests })
}

/// Parse `metric op value` (e.g., `bandwidth above 900mbit`, `avg below 5ms`).
/// Operators: `above` (>), `below` (<).
fn parse_benchmark_assertion(
    tokens: &[Spanned],
    pos: &mut usize,
) -> Result<ast::BenchmarkAssertionDef> {
    let metric = expect_ident(tokens, pos)?;
    let op = expect_ident(tokens, pos)?;
    let value = parse_value(tokens, pos)?;
    Ok(ast::BenchmarkAssertionDef { metric, op, value })
}

// ─── Site ─────────────────────────────────────────────────

fn parse_site(tokens: &[Spanned], pos: &mut usize) -> Result<ast::SiteDef> {
    expect_kw(tokens, pos, "site")?;
    let name = expect_ident(tokens, pos)?;
    let description = if matches!(at(tokens, *pos), Some(Token::String(_))) {
        Some(expect_string(tokens, pos)?)
    } else {
        None
    };

    expect(tokens, pos, &Token::LBrace)?;
    let mut body = Vec::new();
    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }
        body.push(parse_statement(tokens, pos)?);
    }

    Ok(ast::SiteDef {
        name,
        description,
        body,
    })
}

fn parse_param(tokens: &[Spanned], pos: &mut usize) -> Result<ast::ParamDef> {
    expect(tokens, pos, &Token::Param)?;
    let name = expect_ident(tokens, pos)?;
    let default = if eat_kw(tokens, pos, "default") {
        Some(parse_value(tokens, pos)?)
    } else {
        None
    };
    Ok(ast::ParamDef { name, default })
}

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

    let range = if check(tokens, *pos, &Token::LBracket) {
        // List iteration: for x in [a, b, c]
        *pos += 1;
        let mut items = Vec::new();
        loop {
            skip_newlines(tokens, pos);
            if check(tokens, *pos, &Token::RBracket) {
                *pos += 1;
                break;
            }
            items.push(parse_value(tokens, pos)?);
            eat(tokens, pos, &Token::Comma);
        }
        ast::ForRange::List(items)
    } else {
        // Integer range: for i in 1..4
        let start = expect_int(tokens, pos)?;
        expect(tokens, pos, &Token::DotDot)?;
        let end = expect_int(tokens, pos)?;
        ast::ForRange::IntRange { start, end }
    };

    expect(tokens, pos, &Token::LBrace)?;
    let mut body = Vec::new();

    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }
        body.push(parse_statement(tokens, pos)?);
    }

    Ok(ast::ForLoop { var, range, body })
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

    // For-expression: [for var in start..end : template]
    if check(tokens, *pos, &Token::For) {
        return parse_for_expr(tokens, pos);
    }

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

    // For-expression: [for i in 1..4 : r${i}:mgmt0]
    if check(tokens, *pos, &Token::For) {
        return parse_for_expr(tokens, pos);
    }

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

/// Parse a for-expression inside brackets: `for var in start..end : template`.
/// The opening `[` has already been consumed.
fn parse_for_expr(tokens: &[Spanned], pos: &mut usize) -> Result<Vec<String>> {
    expect(tokens, pos, &Token::For)?;
    let var = expect_ident(tokens, pos)?;
    expect(tokens, pos, &Token::In)?;
    let start = expect_int(tokens, pos)?;
    expect(tokens, pos, &Token::DotDot)?;
    let end = expect_int(tokens, pos)?;
    expect(tokens, pos, &Token::Colon)?;

    // Collect remaining tokens until ] as the template string
    let mut template_parts = Vec::new();
    while *pos < tokens.len() && !matches!(tokens[*pos].token, Token::RBracket) {
        template_parts.push(parse_name(tokens, pos)?);
    }
    let template = template_parts.join("");
    expect(tokens, pos, &Token::RBracket)?;

    // Expand the for-expression
    let items = (start..=end)
        .map(|i| template.replace(&format!("${{{var}}}"), &i.to_string()))
        .collect();
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
    fn test_parse_lab_dns_hosts() {
        let ast = parse_nll(r#"lab "test" { dns hosts }"#);
        assert_eq!(ast.lab.dns.as_deref(), Some("hosts"));
    }

    #[test]
    fn test_parse_lab_dns_off() {
        let ast = parse_nll(r#"lab "test" { dns off }"#);
        assert_eq!(ast.lab.dns.as_deref(), Some("off"));
    }

    #[test]
    fn test_parse_bare_node() {
        let ast = parse_nll(
            r#"lab "t"
node host"#,
        );
        assert_eq!(ast.statements.len(), 1);
        match &ast.statements[0] {
            ast::Statement::Node(n) => {
                assert_eq!(n.name, "host");
                assert!(n.profiles.is_empty());
                assert!(n.props.is_empty());
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_node_with_profile() {
        let ast = parse_nll(
            r#"lab "t"
node r1 : router"#,
        );
        match &ast.statements[0] {
            ast::Statement::Node(n) => {
                assert_eq!(n.name, "r1");
                assert_eq!(n.profiles, vec!["router"]);
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_container_properties() {
        let ast = parse_nll(
            r#"lab "t"
node web image "nginx" {
    cpu 0.5
    memory 256m
    hostname "web-01"
    workdir "/app"
    entrypoint "/bin/sh"
    labels ["role=web", "tier=frontend"]
    pull always
    privileged
    exec "nginx -t"
    exec "echo ready"
}"#,
        );
        match &ast.statements[0] {
            ast::Statement::Node(n) => {
                assert_eq!(n.image.as_deref(), Some("nginx"));
                assert_eq!(n.cpu.as_deref(), Some("0.5"));
                assert_eq!(n.memory.as_deref(), Some("256m")); // parsed from string
                assert_eq!(n.hostname.as_deref(), Some("web-01"));
                assert_eq!(n.workdir.as_deref(), Some("/app"));
                assert_eq!(n.entrypoint.as_deref(), Some("/bin/sh"));
                assert_eq!(n.labels, vec!["role=web", "tier=frontend"]);
                assert_eq!(n.pull.as_deref(), Some("always"));
                assert!(n.privileged);
                assert_eq!(n.container_exec, vec!["nginx -t", "echo ready"]);
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_container_lifecycle() {
        let ast = parse_nll(
            r#"lab "t"
node db image "postgres" {
    healthcheck "pg_isready"
    startup-delay 5s
    env-file "db.env"
    config "pg.conf" "/etc/postgresql/postgresql.conf"
    overlay "configs/db/"
    depends-on [cache]
}
node cache image "redis""#,
        );
        match &ast.statements[0] {
            ast::Statement::Node(n) => {
                assert_eq!(n.healthcheck.as_deref(), Some("pg_isready"));
                assert_eq!(n.startup_delay.as_deref(), Some("5s"));
                assert_eq!(n.env_file.as_deref(), Some("db.env"));
                assert_eq!(
                    n.configs,
                    vec![(
                        "pg.conf".to_string(),
                        "/etc/postgresql/postgresql.conf".to_string()
                    )]
                );
                assert_eq!(n.overlay.as_deref(), Some("configs/db/"));
                assert_eq!(n.depends_on, vec!["cache"]);
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_container_capabilities() {
        let ast = parse_nll(
            r#"lab "t"
node router image "frr" {
    cap-add [NET_ADMIN, NET_RAW, SYS_PTRACE]
    cap-drop [MKNOD]
}"#,
        );
        match &ast.statements[0] {
            ast::Statement::Node(n) => {
                assert_eq!(n.cap_add, vec!["NET_ADMIN", "NET_RAW", "SYS_PTRACE"]);
                assert_eq!(n.cap_drop, vec!["MKNOD"]);
                assert!(!n.privileged);
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_node_with_forward() {
        let ast = parse_nll(
            r#"lab "t"
node r1 { forward ipv4 }"#,
        );
        match &ast.statements[0] {
            ast::Statement::Node(n) => {
                assert_eq!(n.props.len(), 1);
                assert!(matches!(
                    n.props[0],
                    ast::NodeProp::Forward(ast::IpVersion::Ipv4)
                ));
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_node_with_route() {
        let ast = parse_nll(
            r#"lab "t"
node h1 { route default via 10.0.0.1 }"#,
        );
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
        let ast = parse_nll(
            r#"lab "t"
link r1:eth0 -- r2:eth0"#,
        );
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
                match &f.range {
                    ast::ForRange::IntRange { start, end } => {
                        assert_eq!(*start, 1);
                        assert_eq!(*end, 4);
                    }
                    _ => panic!("expected IntRange"),
                }
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
    fn test_parse_firewall_src_dst() {
        let ast = parse_nll(
            r#"lab "t"
node server {
  firewall policy drop {
    accept tcp dport 443
    accept tcp dport 80 src 10.0.0.0/8
    drop src 192.168.0.0/16
    accept dst 10.0.0.1/32
    accept src fd00::/64 tcp dport 22
  }
}"#,
        );
        match &ast.statements[0] {
            ast::Statement::Node(n) => match &n.props[0] {
                ast::NodeProp::Firewall(fw) => {
                    assert_eq!(fw.rules.len(), 5);
                    assert_eq!(fw.rules[0].match_expr, "tcp dport 443");
                    assert_eq!(fw.rules[1].match_expr, "ip saddr 10.0.0.0/8 tcp dport 80");
                    assert_eq!(fw.rules[2].match_expr, "ip saddr 192.168.0.0/16");
                    assert_eq!(fw.rules[3].match_expr, "ip daddr 10.0.0.1/32");
                    assert_eq!(fw.rules[4].match_expr, "ip6 saddr fd00::/64 tcp dport 22");
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
    fn test_parse_macvlan() {
        let ast = parse_nll(
            r#"lab "t"
node gw {
  macvlan eth0 parent "enp3s0" mode bridge {
    192.168.1.100/24
  }
}"#,
        );
        match &ast.statements[0] {
            ast::Statement::Node(n) => match &n.props[0] {
                ast::NodeProp::Macvlan(m) => {
                    assert_eq!(m.name, "eth0");
                    assert_eq!(m.parent, "enp3s0");
                    assert_eq!(m.mode.as_deref(), Some("bridge"));
                    assert_eq!(m.addresses, vec!["192.168.1.100/24"]);
                }
                _ => panic!("expected Macvlan"),
            },
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_ipvlan() {
        let ast = parse_nll(
            r#"lab "t"
node router {
  ipvlan eth0 parent "enp3s0" mode l3
}"#,
        );
        match &ast.statements[0] {
            ast::Statement::Node(n) => match &n.props[0] {
                ast::NodeProp::Ipvlan(iv) => {
                    assert_eq!(iv.name, "eth0");
                    assert_eq!(iv.parent, "enp3s0");
                    assert_eq!(iv.mode.as_deref(), Some("l3"));
                }
                _ => panic!("expected Ipvlan"),
            },
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_wifi_ap() {
        let ast = parse_nll(
            r#"lab "t"
node ap {
  wifi wlan0 mode ap {
    ssid "testnet"
    channel 6
    wpa2 "secret"
    10.0.0.1/24
  }
}"#,
        );
        match &ast.statements[0] {
            ast::Statement::Node(n) => match &n.props[0] {
                ast::NodeProp::Wifi(w) => {
                    assert_eq!(w.name, "wlan0");
                    assert_eq!(w.mode, "ap");
                    assert_eq!(w.ssid.as_deref(), Some("testnet"));
                    assert_eq!(w.channel, Some(6));
                    assert_eq!(w.passphrase.as_deref(), Some("secret"));
                    assert_eq!(w.addresses, vec!["10.0.0.1/24"]);
                }
                _ => panic!("expected Wifi"),
            },
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_wifi_station() {
        let ast = parse_nll(
            r#"lab "t"
node sta {
  wifi wlan0 mode station {
    ssid "testnet"
    wpa2 "secret"
  }
}"#,
        );
        match &ast.statements[0] {
            ast::Statement::Node(n) => match &n.props[0] {
                ast::NodeProp::Wifi(w) => {
                    assert_eq!(w.mode, "station");
                    assert_eq!(w.ssid.as_deref(), Some("testnet"));
                    assert!(w.channel.is_none());
                }
                _ => panic!("expected Wifi"),
            },
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn test_parse_wifi_mesh() {
        let ast = parse_nll(
            r#"lab "t"
node m {
  wifi wlan0 mode mesh {
    mesh-id "labmesh"
    channel 1
  }
}"#,
        );
        match &ast.statements[0] {
            ast::Statement::Node(n) => match &n.props[0] {
                ast::NodeProp::Wifi(w) => {
                    assert_eq!(w.mode, "mesh");
                    assert_eq!(w.mesh_id.as_deref(), Some("labmesh"));
                }
                _ => panic!("expected Wifi"),
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
