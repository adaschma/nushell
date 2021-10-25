use nu_engine::CallExt;
use nu_protocol::ast::Call;
use nu_protocol::engine::{Command, EvaluationContext};
use nu_protocol::{IntoPipelineData, PipelineData, Signature, SyntaxShape, Value};

#[derive(Clone)]
pub struct Wrap;

impl Command for Wrap {
    fn name(&self) -> &str {
        "wrap"
    }

    fn usage(&self) -> &str {
        "Wrap the value into a column."
    }

    fn signature(&self) -> nu_protocol::Signature {
        Signature::build("wrap").required("name", SyntaxShape::String, "the name of the column")
    }

    fn run(
        &self,
        context: &EvaluationContext,
        call: &Call,
        input: PipelineData,
    ) -> Result<nu_protocol::PipelineData, nu_protocol::ShellError> {
        let span = call.head;
        let name: String = call.req(context, 0)?;

        match input {
            PipelineData::Value(Value::List { vals, .. }) => Ok(vals
                .into_iter()
                .map(move |x| Value::Record {
                    cols: vec![name.clone()],
                    vals: vec![x],
                    span,
                })
                .into_pipeline_data()),
            PipelineData::Stream(stream) => Ok(stream
                .map(move |x| Value::Record {
                    cols: vec![name.clone()],
                    vals: vec![x],
                    span,
                })
                .into_pipeline_data()),
            PipelineData::Value(input) => Ok(Value::Record {
                cols: vec![name],
                vals: vec![input],
                span,
            }
            .into_pipeline_data()),
        }
    }
}
