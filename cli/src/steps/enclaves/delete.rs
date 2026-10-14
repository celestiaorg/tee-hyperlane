//! `teeism enclaves delete`: delete a CVM on Phala, by id or every retired one.

use anyhow::{bail, Result};

use crate::shared::print;
use crate::shared::settings::Ctx;
use crate::shared::shell::Cmd;

/// `teeism enclaves delete`
pub fn run(ctx: &Ctx, app_id: Option<String>, retired: bool) -> Result<()> {
    match (app_id, retired) {
        (_, true) => delete_retired(ctx),
        (Some(id), false) => delete(&id),
        (None, false) => bail!("give --app-id <id> or --retired"),
    }
}

/// `teeism enclaves delete --retired`: delete every CVM a newer one replaced.
pub fn delete_retired(ctx: &Ctx) -> Result<()> {
    let mut d = ctx.deployment()?;
    if d.enclaves.retired.is_empty() {
        print::info("no retired CVMs");
        return Ok(());
    }
    while let Some(cvm) = d.enclaves.retired.first().cloned() {
        print::step(format!("deleting retired CVM {} ({})", cvm.app_id, cvm.name));
        delete(&cvm.app_id)?;
        d.enclaves.retired.remove(0);
        ctx.save(&d)?;
    }
    Ok(())
}

/// Delete a CVM on Phala.
pub fn delete(app_id: &str) -> Result<()> {
    Cmd::new(format!("deleting CVM {app_id} on Phala"), "phala")
        .args(["cvms", "delete", app_id, "--force"])
        .output()?;
    Ok(())
}
