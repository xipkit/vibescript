use super::{
    calls::Location,
    entry::Check,
    public::{CheckDiagnostic, CheckReport},
};
use crate::{
    CallContext, Result, Stats,
    budget::{Buffer, Charge},
    bytecode::Program,
};
use std::cmp::Ordering;

mod message;
#[cfg(test)]
mod tests;
mod types;

pub(super) fn build(
    ctx: &mut CallContext,
    program: &Program,
    check: &Check,
) -> Result<CheckReport> {
    let mut diagnostics = Buffer::empty();
    for issue in &check.analysis.issues.data {
        ctx.charge(1)?;
        let code = check.facts.source_code(ctx, issue.source)?;
        let program = code.as_ref().map_or(program, |code| &code.program);
        let (message, charge) = message::issue(ctx, program, &check.facts, issue)?;
        let diagnostic = locate(
            ctx,
            program,
            Location {
                source: issue.source,
                function: issue.function,
                pc: issue.issue.pc,
            },
            check.entry,
            message,
            charge,
        )?;
        diagnostics.push(ctx, diagnostic)?;
    }
    let mut incomplete = Buffer::empty();
    for &location in &check.analysis.incomplete.data {
        ctx.charge(1)?;
        let code = check.facts.source_code(ctx, location.source)?;
        let program = code.as_ref().map_or(program, |code| &code.program);
        let mut writer = types::Writer::new(ctx);
        match &check.pending {
            Some(super::entry::Pending::Capability(name)) => {
                writer.quoted(name.as_bytes().unwrap())?;
                writer.text(": capability factory analysis is not implemented")?;
            }
            Some(super::entry::Pending::Message(message)) => writer.text(message)?,
            None => writer.text("Analysis of this expression is not implemented")?,
        }
        let (message, charge) = writer.finish();
        let diagnostic = locate(ctx, program, location, check.entry, message, charge)?;
        incomplete.push(ctx, diagnostic)?;
    }
    order(ctx, &mut diagnostics.data)?;
    order(ctx, &mut incomplete.data)?;
    let (diagnostics, mut charge) = diagnostics.into_parts();
    let (incomplete, other) = incomplete.into_parts();
    Charge::merge(&mut charge, other);
    Ok(CheckReport {
        diagnostics,
        incomplete,
        stats: Stats::default(),
        _charge: charge,
    })
}

fn locate(
    ctx: &mut CallContext,
    program: &Program,
    location: Location,
    entry: bool,
    message: String,
    mut charge: Option<Charge>,
) -> Result<CheckDiagnostic> {
    let function = &program.functions[location.function];
    let offset = if entry {
        function.offset
    } else {
        function
            .locations
            .get(location.pc)
            .copied()
            .unwrap_or(function.offset)
    };
    let position = program.source.position_metered(ctx, offset)?;
    let (code_frame, frame_charge) = program.source.frame_metered(ctx, offset, position)?;
    Charge::merge(&mut charge, frame_charge);
    let (function, name_charge) =
        crate::source::formatted(ctx, format_args!("{}", function.trace_name))?;
    Charge::merge(&mut charge, name_charge);
    if let Some(filename) = &program.source.filename {
        ctx.work_bytes(filename.len())?;
        Charge::merge(&mut charge, ctx.reserve(filename.len())?);
    }
    Ok(CheckDiagnostic {
        function,
        filename: program.source.filename.clone(),
        offset: offset as usize,
        position,
        message,
        code_frame,
        _source: location.source,
        _charge: charge,
    })
}

fn compare(ctx: &mut CallContext, a: &CheckDiagnostic, b: &CheckDiagnostic) -> Result<Ordering> {
    ctx.charge(1)?;
    if let (Some(left), Some(right)) = (&a.filename, &b.filename) {
        ctx.work_bytes(left.len().min(right.len()))?;
    }
    let source = a
        .filename
        .cmp(&b.filename)
        .then_with(|| a._source.cmp(&b._source));
    if source != Ordering::Equal {
        return Ok(source);
    }
    let offset = a.offset.cmp(&b.offset);
    if offset != Ordering::Equal {
        return Ok(offset);
    }
    ctx.work_bytes(a.function.len().min(b.function.len()))?;
    let function = a.function.cmp(&b.function);
    if function != Ordering::Equal {
        return Ok(function);
    }
    ctx.work_bytes(a.message.len().min(b.message.len()))?;
    Ok(a.message.cmp(&b.message))
}

fn order(ctx: &mut CallContext, items: &mut Vec<CheckDiagnostic>) -> Result<()> {
    let mut sort = crate::sort::Sort::new(items.len());
    let mut comparison = None;
    loop {
        match sort.advance(ctx, comparison.take())? {
            crate::sort::Action::Compare(a, b) => {
                comparison = Some(compare(ctx, &items[a], &items[b])?)
            }
            crate::sort::Action::Swap(a, b) => items.swap(a, b),
            crate::sort::Action::Done => break,
        }
    }
    let mut keep = 0;
    for index in 0..items.len() {
        if keep == 0 || compare(ctx, &items[keep - 1], &items[index])? != Ordering::Equal {
            items.swap(keep, index);
            keep += 1;
        }
    }
    items.truncate(keep);
    Ok(())
}
