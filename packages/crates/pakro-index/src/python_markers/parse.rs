//! Bounded tokenization and iterative construction of the unmodified syntax.

use super::*;

enum TokenKind {
    Identifier,
    Literal(Range<usize>),
    Operator(PythonMarkerOperator),
    And,
    Or,
    Open,
    Close,
}
struct Token {
    kind: TokenKind,
    span: Range<usize>,
}
#[derive(Clone, Copy)]
enum Pending {
    Open { offset: usize, height: usize },
    And(usize),
    Or(usize),
}
impl Pending {
    fn precedence(self) -> usize {
        match self {
            Self::And(_) => 2,
            Self::Or(_) => 1,
            Self::Open { .. } => 0,
        }
    }
    fn offset(self) -> usize {
        match self {
            Self::And(offset) | Self::Or(offset) | Self::Open { offset, .. } => offset,
        }
    }
}

pub(super) fn validate_limits(limits: PythonMarkerLimits) -> PythonMarkerResult<()> {
    if [
        limits.max_input_bytes,
        limits.max_literal_bytes,
        limits.max_tokens,
        limits.max_nodes,
        limits.max_group_depth,
        limits.max_environment_bytes,
        limits.max_environment_field_bytes,
        limits.max_set_entries,
        limits.max_evaluation_steps,
    ]
    .contains(&0)
    {
        return Err(PythonMarkerError::InvalidLimits);
    }
    Ok(())
}

fn limit(kind: PythonMarkerLimitKind) -> PythonMarkerError {
    PythonMarkerError::LimitExceeded(kind)
}
fn syntax(offset: usize) -> PythonMarkerError {
    PythonMarkerError::Syntax { offset }
}
fn identifier(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn tokenize(
    raw: &str,
    limits: PythonMarkerLimits,
    max_comparisons: Option<usize>,
) -> PythonMarkerResult<(Vec<Token>, usize)> {
    if raw.len() > limits.max_input_bytes {
        return Err(limit(PythonMarkerLimitKind::InputBytes));
    }
    let bytes = raw.as_bytes();
    let mut tokens = Vec::new();
    let mut position = 0usize;
    let mut depth = 0usize;
    let mut max_depth = 0usize;
    let mut comparisons = 0usize;
    while position < bytes.len() {
        let start = position;
        let kind = match bytes[position] {
            b' ' | b'\t' => {
                position += 1;
                continue;
            }
            b'\'' | b'"' => {
                let quote = bytes[position];
                position += 1;
                let value_start = position;
                loop {
                    let Some(&byte) = bytes.get(position) else {
                        return Err(syntax(start));
                    };
                    if byte == quote {
                        break;
                    }
                    let character = raw[position..]
                        .chars()
                        .next()
                        .ok_or_else(|| syntax(position))?;
                    if character == '\\' || (character.is_control() && character != '\t') {
                        return Err(syntax(position));
                    }
                    position += character.len_utf8();
                    if position - value_start > limits.max_literal_bytes {
                        return Err(limit(PythonMarkerLimitKind::LiteralBytes));
                    }
                }
                let value_span = value_start..position;
                position += 1;
                TokenKind::Literal(value_span)
            }
            b'(' => {
                depth = depth
                    .checked_add(1)
                    .ok_or_else(|| limit(PythonMarkerLimitKind::GroupDepth))?;
                if depth > limits.max_group_depth {
                    return Err(limit(PythonMarkerLimitKind::GroupDepth));
                }
                max_depth = max_depth.max(depth);
                position += 1;
                TokenKind::Open
            }
            b')' => {
                depth = depth.checked_sub(1).ok_or_else(|| syntax(start))?;
                position += 1;
                TokenKind::Close
            }
            b'<' | b'>' | b'=' | b'!' | b'~' => {
                let rest = &raw[position..];
                let (length, operator) = if rest.starts_with("===") {
                    (3, PythonMarkerOperator::ArbitraryEqual)
                } else if rest.starts_with("<=") {
                    (2, PythonMarkerOperator::LessEqual)
                } else if rest.starts_with(">=") {
                    (2, PythonMarkerOperator::GreaterEqual)
                } else if rest.starts_with("==") {
                    (2, PythonMarkerOperator::Equal)
                } else if rest.starts_with("!=") {
                    (2, PythonMarkerOperator::NotEqual)
                } else if rest.starts_with("~=") {
                    (2, PythonMarkerOperator::Compatible)
                } else if rest.starts_with('<') {
                    (1, PythonMarkerOperator::Less)
                } else if rest.starts_with('>') {
                    (1, PythonMarkerOperator::Greater)
                } else {
                    return Err(syntax(start));
                };
                position += length;
                TokenKind::Operator(operator)
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                position += 1;
                while bytes.get(position).is_some_and(|byte| identifier(*byte)) {
                    position += 1;
                }
                match &raw[start..position] {
                    "and" => TokenKind::And,
                    "or" => TokenKind::Or,
                    "in" => TokenKind::Operator(PythonMarkerOperator::In),
                    "not" => {
                        if !bytes
                            .get(position)
                            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
                        {
                            return Err(syntax(start));
                        }
                        while bytes
                            .get(position)
                            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
                        {
                            position += 1;
                        }
                        if !raw[position..].starts_with("in") {
                            return Err(syntax(start));
                        }
                        position += 2;
                        if bytes.get(position).is_some_and(|byte| identifier(*byte)) {
                            return Err(syntax(start));
                        }
                        TokenKind::Operator(PythonMarkerOperator::NotIn)
                    }
                    _ => TokenKind::Identifier,
                }
            }
            _ => return Err(syntax(start)),
        };
        if matches!(kind, TokenKind::Operator(_)) {
            comparisons = comparisons
                .checked_add(1)
                .ok_or_else(|| limit(PythonMarkerLimitKind::Comparisons))?;
            if max_comparisons.is_some_and(|maximum| comparisons > maximum) {
                return Err(limit(PythonMarkerLimitKind::Comparisons));
            }
        }
        if tokens.len() >= limits.max_tokens {
            return Err(limit(PythonMarkerLimitKind::Tokens));
        }
        tokens
            .try_reserve(1)
            .map_err(|_| limit(PythonMarkerLimitKind::Tokens))?;
        tokens.push(Token {
            kind,
            span: start..position,
        });
    }
    Ok((tokens, max_depth))
}

fn operand(token: &Token, raw: &str) -> PythonMarkerResult<Operand> {
    match &token.kind {
        TokenKind::Literal(value_span) => Ok(Operand::Literal {
            span: token.span.clone(),
            value_span: value_span.clone(),
        }),
        TokenKind::Identifier => {
            let field = match &raw[token.span.clone()] {
                "os_name" => PythonMarkerField::OsName,
                "sys_platform" => PythonMarkerField::SysPlatform,
                "platform_release" => PythonMarkerField::PlatformRelease,
                "platform_system" => PythonMarkerField::PlatformSystem,
                "platform_version" => PythonMarkerField::PlatformVersion,
                "platform_machine" => PythonMarkerField::PlatformMachine,
                "platform_python_implementation" => PythonMarkerField::PlatformPythonImplementation,
                "python_version" => PythonMarkerField::PythonVersion,
                "python_full_version" => PythonMarkerField::PythonFullVersion,
                "implementation_name" => PythonMarkerField::ImplementationName,
                "implementation_version" => PythonMarkerField::ImplementationVersion,
                "extra" => PythonMarkerField::Extra,
                "extras" => PythonMarkerField::Extras,
                "dependency_groups" => PythonMarkerField::DependencyGroups,
                _ => {
                    return Err(PythonMarkerError::UnknownField {
                        offset: token.span.start,
                    });
                }
            };
            Ok(Operand::Field {
                field,
                span: token.span.clone(),
            })
        }
        _ => Err(syntax(token.span.start)),
    }
}

fn push_node(
    nodes: &mut Vec<MarkerNode>,
    node: MarkerNode,
    limits: PythonMarkerLimits,
) -> PythonMarkerResult<usize> {
    if nodes.len() >= limits.max_nodes {
        return Err(limit(PythonMarkerLimitKind::Nodes));
    }
    nodes
        .try_reserve(1)
        .map_err(|_| limit(PythonMarkerLimitKind::Nodes))?;
    let index = nodes.len();
    nodes.push(node);
    Ok(index)
}
fn reduce(
    operator: Pending,
    values: &mut Vec<usize>,
    nodes: &mut Vec<MarkerNode>,
    limits: PythonMarkerLimits,
) -> PythonMarkerResult<()> {
    let right = values.pop().ok_or_else(|| syntax(operator.offset()))?;
    let left = values.pop().ok_or_else(|| syntax(operator.offset()))?;
    let node = match operator {
        Pending::And(_) => MarkerNode::And(left, right),
        Pending::Or(_) => MarkerNode::Or(left, right),
        Pending::Open { offset, .. } => return Err(syntax(offset)),
    };
    values.push(push_node(nodes, node, limits)?);
    Ok(())
}

pub(super) fn parse(
    raw: &str,
    limits: PythonMarkerLimits,
) -> PythonMarkerResult<PythonMarkerExpression> {
    validate_limits(limits)?;
    let (tokens, group_depth) = tokenize(raw, limits, None)?;
    let mut nodes = Vec::new();
    let mut values = Vec::new();
    let mut pending = Vec::new();
    values
        .try_reserve(tokens.len())
        .map_err(|_| limit(PythonMarkerLimitKind::Tokens))?;
    pending
        .try_reserve(tokens.len())
        .map_err(|_| limit(PythonMarkerLimitKind::Tokens))?;
    let mut position = 0usize;
    let mut expect_value = true;
    while position < tokens.len() {
        let token = &tokens[position];
        if expect_value {
            if matches!(token.kind, TokenKind::Open) {
                pending.push(Pending::Open {
                    offset: token.span.start,
                    height: values.len(),
                });
                position += 1;
                continue;
            }
            let left = operand(token, raw)?;
            let operation = tokens.get(position + 1).ok_or_else(|| syntax(raw.len()))?;
            let TokenKind::Operator(operator) = operation.kind else {
                return Err(syntax(operation.span.start));
            };
            let right_token = tokens.get(position + 2).ok_or_else(|| syntax(raw.len()))?;
            let right = operand(right_token, raw)?;
            let node = MarkerNode::Comparison(Comparison {
                left,
                operator,
                operator_span: operation.span.clone(),
                right,
                span: token.span.start..right_token.span.end,
            });
            values.push(push_node(&mut nodes, node, limits)?);
            position += 3;
            expect_value = false;
            continue;
        }
        match token.kind {
            TokenKind::And | TokenKind::Or => {
                let next = if matches!(token.kind, TokenKind::And) {
                    Pending::And(token.span.start)
                } else {
                    Pending::Or(token.span.start)
                };
                while pending
                    .last()
                    .is_some_and(|operator| operator.precedence() >= next.precedence())
                {
                    reduce(
                        pending.pop().ok_or_else(|| syntax(token.span.start))?,
                        &mut values,
                        &mut nodes,
                        limits,
                    )?;
                }
                pending.push(next);
                position += 1;
                expect_value = true;
            }
            TokenKind::Close => {
                while pending
                    .last()
                    .is_some_and(|operator| !matches!(operator, Pending::Open { .. }))
                {
                    reduce(
                        pending.pop().ok_or_else(|| syntax(token.span.start))?,
                        &mut values,
                        &mut nodes,
                        limits,
                    )?;
                }
                let Some(Pending::Open { height, .. }) = pending.pop() else {
                    return Err(syntax(token.span.start));
                };
                if values.len() != height + 1 {
                    return Err(syntax(token.span.start));
                }
                let child = values.pop().ok_or_else(|| syntax(token.span.start))?;
                values.push(push_node(&mut nodes, MarkerNode::Group(child), limits)?);
                position += 1;
            }
            _ => return Err(syntax(token.span.start)),
        }
    }
    if expect_value {
        return Err(syntax(raw.len()));
    }
    while let Some(operator) = pending.pop() {
        reduce(operator, &mut values, &mut nodes, limits)?;
    }
    if values.len() != 1 {
        return Err(syntax(raw.len()));
    }
    let root = values.pop().ok_or_else(|| syntax(raw.len()))?;
    let mut owned = String::new();
    owned
        .try_reserve_exact(raw.len())
        .map_err(|_| limit(PythonMarkerLimitKind::InputBytes))?;
    owned.push_str(raw);
    Ok(PythonMarkerExpression {
        raw: owned,
        nodes,
        root,
        token_count: tokens.len(),
        group_depth,
    })
}

pub(super) fn preflight(
    raw: &str,
    limits: PythonMarkerLimits,
    max_comparisons: usize,
) -> PythonMarkerResult<usize> {
    validate_limits(limits)?;
    if max_comparisons == 0 {
        return Err(PythonMarkerError::InvalidLimits);
    }
    let (tokens, _) = tokenize(raw, limits, Some(max_comparisons))?;
    Ok(tokens.len())
}
