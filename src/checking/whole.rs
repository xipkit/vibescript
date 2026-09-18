use super::{
    calls,
    entry::{self, Check, Pending},
    environment::{Environment, Incomplete},
    facts::Facts,
    inputs::Values,
    public::CheckReport,
    report,
};
use crate::{CallContext, CallOptions, Result, Script};

pub(super) fn check(
    ctx: &mut CallContext,
    script: &Script,
    options: &CallOptions,
) -> Result<CheckReport> {
    ctx.checkpoint()?;
    let program = &script.inner.code.program;
    let mut facts = Facts::new(ctx)?;
    let environment = Environment::new(ctx, &mut facts, script, options)?;
    if let Some(reason) = environment.incomplete.data.first() {
        ctx.charge(1)?;
        let reason = match reason {
            Incomplete::Capability(name) => Pending::Capability(name.clone()),
        };
        let check = entry::unfinished(ctx, facts, 0, reason)?;
        return report::build(ctx, program, &check);
    }
    let mut values = Values::new();
    values.writers = Some([
        script.inner.output_writer.is_some(),
        script.inner.error_writer.is_some(),
    ]);
    let analysis = calls::analyze_whole(ctx, &mut facts, environment, values)?;
    let check = Check {
        facts,
        analysis,
        entry: false,
        pending: None,
    };
    report::build(ctx, program, &check)
}
