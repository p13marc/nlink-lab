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
        Token::Import => Some("import".into()),
        Token::As => Some("as".into()),
        Token::Burst => Some("burst".into()),
        Token::Env => Some("env".into()),
        Token::Volumes => Some("volumes".into()),
        Token::Runtime => Some("runtime".into()),
        Token::Parent => Some("parent".into()),
        Token::Src => Some("src".into()),
        Token::Dst => Some("dst".into()),
        Token::Defaults => Some("defaults".into()),
        Token::Version => Some("version".into()),
        Token::Author => Some("author".into()),
        Token::Tags => Some("tags".into()),
        Token::Cpu => Some("cpu".into()),
        Token::Privileged => Some("privileged".into()),
        Token::CapAdd => Some("cap-add".into()),
        Token::CapDrop => Some("cap-drop".into()),
        Token::Entrypoint => Some("entrypoint".into()),
        Token::Hostname => Some("hostname".into()),
        Token::Workdir => Some("workdir".into()),
        Token::Labels => Some("labels".into()),
        Token::Pull => Some("pull".into()),
        Token::Memory => Some("memory".into()),
        Token::Exec => Some("exec".into()),
        Token::Healthcheck => Some("healthcheck".into()),
        Token::StartupDelay => Some("startup-delay".into()),
        Token::EnvFile => Some("env-file".into()),
        Token::Config => Some("config".into()),
        Token::Overlay => Some("overlay".into()),
        Token::DependsOn => Some("depends-on".into()),
        Token::Interval => Some("interval".into()),
        Token::Timeout => Some("timeout".into()),
        Token::Retries => Some("retries".into()),
        Token::Mgmt => Some("mgmt".into()),
        Token::Subnet => Some("subnet".into()),
        Token::Pool => Some("pool".into()),
        Token::Validate => Some("validate".into()),
        Token::Reach => Some("reach".into()),
        Token::NoReach => Some("no-reach".into()),
        Token::Mesh => Some("mesh".into()),
        Token::Ring => Some("ring".into()),
        Token::Star => Some("star".into()),
        Token::Hub => Some("hub".into()),
        Token::Spokes => Some("spokes".into()),
        Token::Count => Some("count".into()),
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

    loop {
        if *pos >= tokens.len() {
            break;
        }
        match &tokens[*pos].token {
            _ if !started && token_as_ident(&tokens[*pos].token).is_some() => {
                name.push_str(&token_as_ident(&tokens[*pos].token).unwrap());
                *pos += 1;
                started = true;
            }
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
            Token::Dot if started => {
                // Dots allowed for import prefixes (e.g. `dc.r1`)
                name.push('.');
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
        Token::Default => "default".into(),
        Token::Auto => "auto".into(),
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
                Some(Token::Version) => {
                    *pos += 1;
                    version = Some(expect_string(tokens, pos)?);
                }
                Some(Token::Author) => {
                    *pos += 1;
                    author = Some(expect_string(tokens, pos)?);
                }
                Some(Token::Tags) => {
                    *pos += 1;
                    tags = parse_ident_list(tokens, pos)?;
                }
                Some(Token::Mgmt) => {
                    *pos += 1;
                    mgmt = Some(parse_cidr_or_name(tokens, pos)?);
                }
                Some(Token::Dns) => {
                    *pos += 1;
                    dns = Some(expect_ident(tokens, pos)?);
                }
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
        Token::Param => parse_param(tokens, pos).map(ast::Statement::Param),
        Token::Let => parse_let(tokens, pos).map(ast::Statement::Let),
        Token::For => parse_for(tokens, pos).map(ast::Statement::For),
        other => Err(err(
            tokens,
            *pos,
            format!(
                "expected statement (profile, node, link, network, impair, rate, defaults, pool, validate, param, let, for), found {other}"
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
                Some(Token::Cpu) => {
                    *pos += 1;
                    cpu = Some(parse_value(tokens, pos)?);
                }
                Some(Token::Memory) => {
                    *pos += 1;
                    memory = Some(parse_value(tokens, pos)?);
                }
                Some(Token::Privileged) => {
                    *pos += 1;
                    privileged = true;
                }
                Some(Token::CapAdd) => {
                    *pos += 1;
                    cap_add = parse_ident_list(tokens, pos)?;
                }
                Some(Token::CapDrop) => {
                    *pos += 1;
                    cap_drop = parse_ident_list(tokens, pos)?;
                }
                Some(Token::Entrypoint) => {
                    *pos += 1;
                    entrypoint = Some(expect_string(tokens, pos)?);
                }
                Some(Token::Hostname) => {
                    *pos += 1;
                    hostname = Some(expect_string(tokens, pos)?);
                }
                Some(Token::Workdir) => {
                    *pos += 1;
                    workdir = Some(expect_string(tokens, pos)?);
                }
                Some(Token::Labels) => {
                    *pos += 1;
                    labels = parse_string_list(tokens, pos)?;
                }
                Some(Token::Pull) => {
                    *pos += 1;
                    pull = Some(parse_value(tokens, pos)?);
                }
                Some(Token::Exec) => {
                    *pos += 1;
                    container_exec.push(expect_string(tokens, pos)?);
                }
                Some(Token::Healthcheck) => {
                    *pos += 1;
                    healthcheck = Some(expect_string(tokens, pos)?);
                    // Optional inline interval/timeout
                    if eat(tokens, pos, &Token::LBrace) {
                        loop {
                            skip_newlines(tokens, pos);
                            if eat(tokens, pos, &Token::RBrace) {
                                break;
                            }
                            match at(tokens, *pos) {
                                Some(Token::Interval) => {
                                    *pos += 1;
                                    healthcheck_interval = Some(parse_value(tokens, pos)?);
                                }
                                Some(Token::Timeout) => {
                                    *pos += 1;
                                    healthcheck_timeout = Some(parse_value(tokens, pos)?);
                                }
                                Some(Token::Retries) => {
                                    *pos += 1;
                                    // retries stored in timeout field for now
                                    // (can be split later)
                                    let _ = parse_value(tokens, pos)?;
                                }
                                _ => {
                                    // Skip unknown properties
                                    let _ = parse_value(tokens, pos)?;
                                }
                            }
                        }
                    }
                }
                Some(Token::StartupDelay) => {
                    *pos += 1;
                    startup_delay = Some(parse_value(tokens, pos)?);
                }
                Some(Token::EnvFile) => {
                    *pos += 1;
                    env_file = Some(expect_string(tokens, pos)?);
                }
                Some(Token::Config) => {
                    *pos += 1;
                    let host = expect_string(tokens, pos)?;
                    let container = expect_string(tokens, pos)?;
                    configs.push((host, container));
                }
                Some(Token::Overlay) => {
                    *pos += 1;
                    overlay = Some(expect_string(tokens, pos)?);
                }
                Some(Token::DependsOn) => {
                    *pos += 1;
                    depends_on = parse_ident_list(tokens, pos)?;
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
                    return Err(err(
                        tokens,
                        *pos,
                        format!(
                            "expected 'ipv4' or 'ipv6' after 'forward', found {}",
                            other.map_or("end of input".to_string(), |t| t.to_string())
                        ),
                    ));
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
        Some(Token::Macvlan) => {
            *pos += 1;
            parse_macvlan_def(tokens, pos).map(ast::NodeProp::Macvlan)
        }
        Some(Token::Ipvlan) => {
            *pos += 1;
            parse_ipvlan_def(tokens, pos).map(ast::NodeProp::Ipvlan)
        }
        Some(Token::Run) => {
            *pos += 1;
            parse_run_def(tokens, pos).map(ast::NodeProp::Run)
        }
        Some(other) => Err(err(
            tokens,
            *pos,
            format!(
                "expected node property (forward, sysctl, lo, route, firewall, vrf, wireguard, vxlan, dummy, macvlan, ipvlan, run), found {other}"
            ),
        )),
        None => Err(err(
            tokens,
            *pos,
            "unexpected end of input in node block".into(),
        )),
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
            return Err(err(
                tokens,
                *pos,
                format!(
                    "expected firewall policy, found {}",
                    other.map_or("end of input".to_string(), |t| t.to_string())
                ),
            ));
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
            return Err(err(
                tokens,
                *pos,
                format!(
                    "expected firewall action (accept/drop/reject), found {}",
                    other.map_or("end of input".to_string(), |t| t.to_string())
                ),
            ));
        }
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
        match at(tokens, *pos) {
            Some(Token::Src) => {
                *pos += 1;
                let addr = parse_cidr_or_name(tokens, pos)?;
                let family = if addr.contains(':') { "ip6" } else { "ip" };
                parts.insert(0, format!("{family} saddr {addr}")); // saddr first in nftables order
                matched = true;
            }
            Some(Token::Dst) => {
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
            }
            Some(Token::Ct) => {
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
            }
            Some(Token::Tcp) => {
                *pos += 1;
                let dir = match at(tokens, *pos) {
                    Some(Token::Dport) => {
                        *pos += 1;
                        "dport"
                    }
                    Some(Token::Sport) => {
                        *pos += 1;
                        "sport"
                    }
                    other => {
                        return Err(err(
                            tokens,
                            *pos,
                            format!(
                                "expected 'dport' or 'sport' after 'tcp', found {}",
                                other.map_or("end of input".to_string(), |t| t.to_string())
                            ),
                        ));
                    }
                };
                let port = expect_int(tokens, pos)?;
                parts.push(format!("tcp {dir} {port}"));
                matched = true;
            }
            Some(Token::Udp) => {
                *pos += 1;
                let dir = match at(tokens, *pos) {
                    Some(Token::Dport) => {
                        *pos += 1;
                        "dport"
                    }
                    Some(Token::Sport) => {
                        *pos += 1;
                        "sport"
                    }
                    other => {
                        return Err(err(
                            tokens,
                            *pos,
                            format!(
                                "expected 'dport' or 'sport' after 'udp', found {}",
                                other.map_or("end of input".to_string(), |t| t.to_string())
                            ),
                        ));
                    }
                };
                let port = expect_int(tokens, pos)?;
                parts.push(format!("udp {dir} {port}"));
                matched = true;
            }
            Some(Token::Icmp) => {
                *pos += 1;
                let icmp_type = expect_int(tokens, pos)?;
                parts.push(format!("icmp type {icmp_type}"));
                matched = true;
            }
            Some(Token::Icmpv6) => {
                *pos += 1;
                let icmp_type = expect_int(tokens, pos)?;
                parts.push(format!("icmpv6 type {icmp_type}"));
                matched = true;
            }
            Some(Token::Mark) => {
                *pos += 1;
                let mark = expect_int(tokens, pos)?;
                parts.push(format!("mark {mark}"));
                matched = true;
            }
            _ => break,
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

    Ok(ast::DummyDef { name, addresses })
}

// ─── Macvlan ─────────────────────────────────────────────

// macvlan IDENT parent STRING (mode IDENT)? block?
fn parse_macvlan_def(tokens: &[Spanned], pos: &mut usize) -> Result<ast::MacvlanDef> {
    let name = expect_ident(tokens, pos)?;
    expect(tokens, pos, &Token::Parent)?;
    let parent = parse_value(tokens, pos)?;
    let mut mode = None;
    let mut addresses = Vec::new();

    // Inline mode before block
    if matches!(at(tokens, *pos), Some(Token::Ident(s)) if s == "mode") {
        *pos += 1;
        mode = Some(expect_ident(tokens, pos)?);
    }

    if eat(tokens, pos, &Token::LBrace) {
        loop {
            skip_newlines(tokens, pos);
            if eat(tokens, pos, &Token::RBrace) {
                break;
            }
            match at(tokens, *pos) {
                Some(Token::Ident(s)) if s == "mode" => {
                    *pos += 1;
                    mode = Some(expect_ident(tokens, pos)?);
                }
                Some(Token::Address) => {
                    *pos += 1;
                    addresses.push(parse_cidr_or_name(tokens, pos)?);
                }
                Some(Token::Cidr(c)) => {
                    addresses.push(c.clone());
                    *pos += 1;
                }
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
    expect(tokens, pos, &Token::Parent)?;
    let parent = parse_value(tokens, pos)?;
    let mut mode = None;
    let mut addresses = Vec::new();

    if matches!(at(tokens, *pos), Some(Token::Ident(s)) if s == "mode") {
        *pos += 1;
        mode = Some(expect_ident(tokens, pos)?);
    }

    if eat(tokens, pos, &Token::LBrace) {
        loop {
            skip_newlines(tokens, pos);
            if eat(tokens, pos, &Token::RBrace) {
                break;
            }
            match at(tokens, *pos) {
                Some(Token::Ident(s)) if s == "mode" => {
                    *pos += 1;
                    mode = Some(expect_ident(tokens, pos)?);
                }
                Some(Token::Address) => {
                    *pos += 1;
                    addresses.push(parse_cidr_or_name(tokens, pos)?);
                }
                Some(Token::Cidr(c)) => {
                    addresses.push(c.clone());
                    *pos += 1;
                }
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

    Ok(ast::IpvlanDef {
        name,
        parent,
        mode,
        addresses,
    })
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
        subnet: None,
        pool: None,
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
                Some(Token::Subnet) => {
                    *pos += 1;
                    link.subnet = Some(parse_cidr_or_name(tokens, pos)?);
                }
                // MTU
                Some(Token::Mtu) => {
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
                Some(Token::Delay) | Some(Token::Jitter) | Some(Token::Loss)
                | Some(Token::Corrupt) | Some(Token::Reorder) => {
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
        match at(tokens, *pos) {
            Some(Token::Delay) => {
                *pos += 1;
                props.delay = Some(expect_duration_or_value(tokens, pos)?);
            }
            Some(Token::Jitter) => {
                *pos += 1;
                props.jitter = Some(expect_duration_or_value(tokens, pos)?);
            }
            Some(Token::Loss) => {
                *pos += 1;
                props.loss = Some(expect_percent_or_value(tokens, pos)?);
            }
            Some(Token::Rate) => {
                *pos += 1;
                props.rate = Some(expect_rate_or_value(tokens, pos)?);
            }
            Some(Token::Corrupt) => {
                *pos += 1;
                props.corrupt = Some(expect_percent_or_value(tokens, pos)?);
            }
            Some(Token::Reorder) => {
                *pos += 1;
                props.reorder = Some(expect_percent_or_value(tokens, pos)?);
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
                net.vlans.push(ast::VlanDef {
                    id,
                    name: vlan_name,
                });
            }
            Some(Token::Port) => {
                *pos += 1;
                let endpoint = parse_name(tokens, pos)?;
                let port_def = parse_port_block(tokens, pos, endpoint)?;
                net.ports.push(port_def);
            }
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

    Ok(net)
}

fn parse_port_block(tokens: &[Spanned], pos: &mut usize, endpoint: String) -> Result<ast::PortDef> {
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
        Some(other) => {
            return Err(err(
                tokens,
                *pos,
                format!("expected link, impair, or rate after defaults, found {other}"),
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
            (ast::DefaultsKind::Link, Some(Token::Mtu)) => {
                *pos += 1;
                def.mtu = Some(expect_int(tokens, pos)? as u32);
            }
            (ast::DefaultsKind::Impair, _) => {
                def.impair = Some(parse_impair_props(tokens, pos)?);
            }
            (ast::DefaultsKind::Rate, _) => {
                def.rate = Some(parse_rate_props(tokens, pos)?);
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
        match at(tokens, *pos) {
            Some(Token::Node) => {
                // nodes [n1, n2, n3]
                *pos += 1;
                nodes = parse_ident_list(tokens, pos)?;
            }
            Some(Token::Count) => {
                *pos += 1;
                count = Some(expect_int(tokens, pos)?);
            }
            Some(Token::Pool) => {
                *pos += 1;
                pool = Some(expect_ident(tokens, pos)?);
            }
            Some(Token::Profile) => {
                *pos += 1;
                profile = Some(expect_ident(tokens, pos)?);
            }
            Some(Token::Hub) => {
                *pos += 1;
                hub = Some(expect_ident(tokens, pos)?);
            }
            Some(Token::Spokes) => {
                *pos += 1;
                spokes = parse_ident_list(tokens, pos)?;
            }
            _ => {
                *pos += 1;
            } // skip unknown
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
    expect(tokens, pos, &Token::LBrace)?;
    let mut assertions = Vec::new();
    loop {
        skip_newlines(tokens, pos);
        if eat(tokens, pos, &Token::RBrace) {
            break;
        }
        match at(tokens, *pos) {
            Some(Token::Reach) => {
                *pos += 1;
                let from = expect_ident(tokens, pos)?;
                let to = expect_ident(tokens, pos)?;
                assertions.push(ast::AssertionDef::Reach { from, to });
            }
            Some(Token::NoReach) => {
                *pos += 1;
                let from = expect_ident(tokens, pos)?;
                let to = expect_ident(tokens, pos)?;
                assertions.push(ast::AssertionDef::NoReach { from, to });
            }
            Some(Token::TcpConnect) => {
                *pos += 1;
                let from = expect_ident(tokens, pos)?;
                let to = expect_ident(tokens, pos)?;
                let port = expect_int(tokens, pos)? as u16;
                let timeout = if eat(tokens, pos, &Token::Timeout) {
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
            }
            Some(Token::LatencyUnder) => {
                *pos += 1;
                let from = expect_ident(tokens, pos)?;
                let to = expect_ident(tokens, pos)?;
                let max = expect_duration_or_value(tokens, pos)?;
                let samples = if eat(tokens, pos, &Token::Samples) {
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
            }
            Some(Token::RouteHas) => {
                *pos += 1;
                let node = expect_ident(tokens, pos)?;
                let destination = parse_value(tokens, pos)?;
                let mut via = None;
                let mut dev = None;
                while matches!(at(tokens, *pos), Some(Token::Via) | Some(Token::Dev)) {
                    match at(tokens, *pos) {
                        Some(Token::Via) => {
                            *pos += 1;
                            via = Some(parse_value(tokens, pos)?);
                        }
                        Some(Token::Dev) => {
                            *pos += 1;
                            dev = Some(expect_ident(tokens, pos)?);
                        }
                        _ => break,
                    }
                }
                assertions.push(ast::AssertionDef::RouteHas {
                    node,
                    destination,
                    via,
                    dev,
                });
            }
            Some(Token::DnsResolves) => {
                *pos += 1;
                let from = expect_ident(tokens, pos)?;
                let name = parse_value(tokens, pos)?;
                let expected_ip = parse_value(tokens, pos)?;
                assertions.push(ast::AssertionDef::DnsResolves {
                    from,
                    name,
                    expected_ip,
                });
            }
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
    Ok(ast::ValidateDef { assertions })
}

fn parse_param(tokens: &[Spanned], pos: &mut usize) -> Result<ast::ParamDef> {
    expect(tokens, pos, &Token::Param)?;
    let name = expect_ident(tokens, pos)?;
    let default = if eat(tokens, pos, &Token::Default) {
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
