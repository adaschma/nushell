use nu_protocol::ast::{Block, Call, Expr, Expression, Operator, Statement};
use nu_protocol::engine::EvaluationContext;
use nu_protocol::{
    IntoPipelineData, PipelineData, Range, ShellError, Span, Spanned, Type, Unit, Value,
};

use crate::get_full_help;

pub fn eval_operator(op: &Expression) -> Result<Operator, ShellError> {
    match op {
        Expression {
            expr: Expr::Operator(operator),
            ..
        } => Ok(operator.clone()),
        Expression { span, expr, .. } => {
            Err(ShellError::UnknownOperator(format!("{:?}", expr), *span))
        }
    }
}

fn eval_call(
    context: &EvaluationContext,
    call: &Call,
    input: PipelineData,
) -> Result<PipelineData, ShellError> {
    let decl = context.engine_state.get_decl(call.decl_id);
    if call.named.iter().any(|(flag, _)| flag.item == "help") {
        let full_help = get_full_help(&decl.signature(), &decl.examples(), context);
        Ok(Value::String {
            val: full_help,
            span: call.head,
        }
        .into_pipeline_data())
    } else if let Some(block_id) = decl.get_block_id() {
        let mut state = context.enter_scope();
        for (arg, param) in call.positional.iter().zip(
            decl.signature()
                .required_positional
                .iter()
                .chain(decl.signature().optional_positional.iter()),
        ) {
            let result = eval_expression(&state, arg)?;
            let var_id = param
                .var_id
                .expect("internal error: all custom parameters must have var_ids");

            state.add_var(var_id, result);
        }

        if let Some(rest_positional) = decl.signature().rest_positional {
            let mut rest_items = vec![];

            for arg in call.positional.iter().skip(
                decl.signature().required_positional.len()
                    + decl.signature().optional_positional.len(),
            ) {
                let result = eval_expression(&state, arg)?;
                rest_items.push(result);
            }

            let span = if let Some(rest_item) = rest_items.first() {
                rest_item.span()?
            } else {
                Span::unknown()
            };

            state.add_var(
                rest_positional
                    .var_id
                    .expect("Internal error: rest positional parameter lacks var_id"),
                Value::List {
                    vals: rest_items,
                    span,
                },
            )
        }

        for named in decl.signature().named {
            if let Some(var_id) = named.var_id {
                let mut found = false;
                for call_named in &call.named {
                    if call_named.0.item == named.long {
                        if let Some(arg) = &call_named.1 {
                            let result = eval_expression(&state, arg)?;

                            state.add_var(var_id, result);
                        } else {
                            state.add_var(
                                var_id,
                                Value::Bool {
                                    val: true,
                                    span: call.head,
                                },
                            )
                        }
                        found = true;
                    }
                }

                if !found && named.arg.is_none() {
                    state.add_var(
                        var_id,
                        Value::Bool {
                            val: false,
                            span: call.head,
                        },
                    )
                }
            }
        }
        let block = context.engine_state.get_block(block_id);
        eval_block(&state, block, input)
    } else {
        decl.run(context, call, input)
    }
}

fn eval_external(
    context: &EvaluationContext,
    name: &str,
    name_span: &Span,
    args: &[Expression],
    input: PipelineData,
    last_expression: bool,
) -> Result<PipelineData, ShellError> {
    let decl_id = context
        .engine_state
        .find_decl("run_external".as_bytes())
        .ok_or_else(|| ShellError::ExternalNotSupported(*name_span))?;

    let command = context.engine_state.get_decl(decl_id);

    let mut call = Call::new();

    call.positional.push(Expression {
        expr: Expr::String(name.trim_start_matches('^').to_string()),
        span: *name_span,
        ty: Type::String,
        custom_completion: None,
    });

    for arg in args {
        call.positional.push(arg.clone())
    }

    if last_expression {
        call.named.push((
            Spanned {
                item: "last_expression".into(),
                span: Span::unknown(),
            },
            None,
        ))
    }

    command.run(context, &call, input)
}

pub fn eval_expression(
    context: &EvaluationContext,
    expr: &Expression,
) -> Result<Value, ShellError> {
    match &expr.expr {
        Expr::Bool(b) => Ok(Value::Bool {
            val: *b,
            span: expr.span,
        }),
        Expr::Int(i) => Ok(Value::Int {
            val: *i,
            span: expr.span,
        }),
        Expr::Float(f) => Ok(Value::Float {
            val: *f,
            span: expr.span,
        }),
        Expr::ValueWithUnit(e, unit) => match eval_expression(context, e)? {
            Value::Int { val, .. } => Ok(compute(val, unit.item, unit.span)),
            _ => Err(ShellError::CantConvert("unit value".into(), e.span)),
        },
        Expr::Range(from, next, to, operator) => {
            let from = if let Some(f) = from {
                eval_expression(context, f)?
            } else {
                Value::Nothing {
                    span: Span::unknown(),
                }
            };

            let next = if let Some(s) = next {
                eval_expression(context, s)?
            } else {
                Value::Nothing {
                    span: Span::unknown(),
                }
            };

            let to = if let Some(t) = to {
                eval_expression(context, t)?
            } else {
                Value::Nothing {
                    span: Span::unknown(),
                }
            };

            Ok(Value::Range {
                val: Box::new(Range::new(expr.span, from, next, to, operator)?),
                span: expr.span,
            })
        }
        Expr::Var(var_id) => context
            .get_var(*var_id)
            .map_err(move |_| ShellError::VariableNotFoundAtRuntime(expr.span)),
        Expr::CellPath(cell_path) => Ok(Value::CellPath {
            val: cell_path.clone(),
            span: expr.span,
        }),
        Expr::FullCellPath(cell_path) => {
            let value = eval_expression(context, &cell_path.head)?;

            value.follow_cell_path(&cell_path.tail)
        }
        Expr::RowCondition(_, expr) => eval_expression(context, expr),
        Expr::Call(call) => {
            // FIXME: protect this collect with ctrl-c
            Ok(Value::List {
                vals: eval_call(context, call, PipelineData::new())?.collect(),
                span: expr.span,
            })
        }
        Expr::ExternalCall(name, span, args) => {
            // FIXME: protect this collect with ctrl-c
            Ok(Value::List {
                vals: eval_external(context, name, span, args, PipelineData::new(), true)?
                    .collect(),
                span: expr.span,
            })
        }
        Expr::Operator(_) => Ok(Value::Nothing { span: expr.span }),
        Expr::BinaryOp(lhs, op, rhs) => {
            let op_span = op.span;
            let lhs = eval_expression(context, lhs)?;
            let op = eval_operator(op)?;
            let rhs = eval_expression(context, rhs)?;

            match op {
                Operator::Plus => lhs.add(op_span, &rhs),
                Operator::Minus => lhs.sub(op_span, &rhs),
                Operator::Multiply => lhs.mul(op_span, &rhs),
                Operator::Divide => lhs.div(op_span, &rhs),
                Operator::LessThan => lhs.lt(op_span, &rhs),
                Operator::LessThanOrEqual => lhs.lte(op_span, &rhs),
                Operator::GreaterThan => lhs.gt(op_span, &rhs),
                Operator::GreaterThanOrEqual => lhs.gte(op_span, &rhs),
                Operator::Equal => lhs.eq(op_span, &rhs),
                Operator::NotEqual => lhs.ne(op_span, &rhs),
                Operator::In => lhs.r#in(op_span, &rhs),
                Operator::NotIn => lhs.not_in(op_span, &rhs),
                Operator::Contains => lhs.contains(op_span, &rhs),
                Operator::NotContains => lhs.not_contains(op_span, &rhs),
                Operator::Modulo => lhs.modulo(op_span, &rhs),
                Operator::And => lhs.and(op_span, &rhs),
                Operator::Or => lhs.or(op_span, &rhs),
                Operator::Pow => lhs.pow(op_span, &rhs),
            }
        }
        Expr::Subexpression(block_id) => {
            let block = context.engine_state.get_block(*block_id);

            let state = context.enter_scope();

            // FIXME: protect this collect with ctrl-c
            Ok(Value::List {
                vals: eval_block(&state, block, PipelineData::new())?.collect(),
                span: expr.span,
            })
        }
        Expr::Block(block_id) => Ok(Value::Block {
            val: *block_id,
            span: expr.span,
        }),
        Expr::List(x) => {
            let mut output = vec![];
            for expr in x {
                output.push(eval_expression(context, expr)?);
            }
            Ok(Value::List {
                vals: output,
                span: expr.span,
            })
        }
        Expr::Table(headers, vals) => {
            let mut output_headers = vec![];
            for expr in headers {
                output_headers.push(eval_expression(context, expr)?.as_string()?);
            }

            let mut output_rows = vec![];
            for val in vals {
                let mut row = vec![];
                for expr in val {
                    row.push(eval_expression(context, expr)?);
                }
                output_rows.push(Value::Record {
                    cols: output_headers.clone(),
                    vals: row,
                    span: expr.span,
                });
            }
            Ok(Value::List {
                vals: output_rows,
                span: expr.span,
            })
        }
        Expr::Keyword(_, _, expr) => eval_expression(context, expr),
        Expr::String(s) => Ok(Value::String {
            val: s.clone(),
            span: expr.span,
        }),
        Expr::Filepath(s) => Ok(Value::String {
            val: s.clone(),
            span: expr.span,
        }),
        Expr::GlobPattern(s) => Ok(Value::String {
            val: s.clone(),
            span: expr.span,
        }),
        Expr::Signature(_) => Ok(Value::Nothing { span: expr.span }),
        Expr::Garbage => Ok(Value::Nothing { span: expr.span }),
    }
}

pub fn eval_block(
    context: &EvaluationContext,
    block: &Block,
    mut input: PipelineData,
) -> Result<PipelineData, ShellError> {
    for stmt in block.stmts.iter() {
        if let Statement::Pipeline(pipeline) = stmt {
            for (i, elem) in pipeline.expressions.iter().enumerate() {
                match elem {
                    Expression {
                        expr: Expr::Call(call),
                        ..
                    } => {
                        input = eval_call(context, call, input)?;
                    }
                    Expression {
                        expr: Expr::ExternalCall(name, name_span, args),
                        ..
                    } => {
                        input = eval_external(
                            context,
                            name,
                            name_span,
                            args,
                            input,
                            i == pipeline.expressions.len() - 1,
                        )?;
                    }

                    elem => {
                        input = eval_expression(context, elem)?.into_pipeline_data();
                    }
                }
            }
        }
    }

    Ok(input)
}

pub fn compute(size: i64, unit: Unit, span: Span) -> Value {
    match unit {
        Unit::Byte => Value::Filesize { val: size, span },
        Unit::Kilobyte => Value::Filesize {
            val: size * 1000,
            span,
        },
        Unit::Megabyte => Value::Filesize {
            val: size * 1000 * 1000,
            span,
        },
        Unit::Gigabyte => Value::Filesize {
            val: size * 1000 * 1000 * 1000,
            span,
        },
        Unit::Terabyte => Value::Filesize {
            val: size * 1000 * 1000 * 1000 * 1000,
            span,
        },
        Unit::Petabyte => Value::Filesize {
            val: size * 1000 * 1000 * 1000 * 1000 * 1000,
            span,
        },

        Unit::Kibibyte => Value::Filesize {
            val: size * 1024,
            span,
        },
        Unit::Mebibyte => Value::Filesize {
            val: size * 1024 * 1024,
            span,
        },
        Unit::Gibibyte => Value::Filesize {
            val: size * 1024 * 1024 * 1024,
            span,
        },
        Unit::Tebibyte => Value::Filesize {
            val: size * 1024 * 1024 * 1024 * 1024,
            span,
        },
        Unit::Pebibyte => Value::Filesize {
            val: size * 1024 * 1024 * 1024 * 1024 * 1024,
            span,
        },

        Unit::Nanosecond => Value::Duration { val: size, span },
        Unit::Microsecond => Value::Duration {
            val: size * 1000,
            span,
        },
        Unit::Millisecond => Value::Duration {
            val: size * 1000 * 1000,
            span,
        },
        Unit::Second => Value::Duration {
            val: size * 1000 * 1000 * 1000,
            span,
        },
        Unit::Minute => Value::Duration {
            val: size * 1000 * 1000 * 1000 * 60,
            span,
        },
        Unit::Hour => Value::Duration {
            val: size * 1000 * 1000 * 1000 * 60 * 60,
            span,
        },
        Unit::Day => Value::Duration {
            val: size * 1000 * 1000 * 1000 * 60 * 60 * 24,
            span,
        },
        Unit::Week => Value::Duration {
            val: size * 1000 * 1000 * 1000 * 60 * 60 * 24 * 7,
            span,
        },
    }
}
