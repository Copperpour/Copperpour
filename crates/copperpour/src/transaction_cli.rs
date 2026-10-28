use anyhow::{bail, Context, Result};
use copperpour_sexp::{
    abandon_file_transaction, inspect_file_transactions, recover_file_transactions,
    TransactionTargetState,
};
use std::path::Path;

pub fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("status") => status(exact_project_path(args, "status")?),
        Some("recover") => recover(exact_project_path(args, "recover")?),
        Some("abandon") => abandon(args),
        Some(command) => {
            bail!("unknown transaction command '{command}'; expected status, recover, or abandon")
        }
        None => bail!("transaction command required; expected status, recover, or abandon"),
    }
}

fn exact_project_path<'a>(args: &'a [String], command: &str) -> Result<&'a Path> {
    if args.len() != 2 {
        bail!("usage: copperpour transaction {command} <project-dir>");
    }
    Ok(Path::new(&args[1]))
}

fn status(project: &Path) -> Result<()> {
    let statuses = inspect_file_transactions(project)
        .with_context(|| format!("failed to inspect transactions in {}", project.display()))?;
    if statuses.is_empty() {
        println!("No active transaction journals.");
        return Ok(());
    }

    for status in statuses {
        println!("Transaction {} ({})", status.id, status.journal.display());
        for target in status.targets {
            let state = match target.state {
                TransactionTargetState::Pending => "pending",
                TransactionTargetState::Applied => "applied",
                TransactionTargetState::Divergent => "divergent",
            };
            println!("  {state:9} {}", target.path.display());
        }
    }
    println!("Journal contents are redacted because they contain complete schematic images.");
    Ok(())
}

fn recover(project: &Path) -> Result<()> {
    let outcomes = recover_file_transactions(project)
        .with_context(|| format!("failed to recover transactions in {}", project.display()))?;
    if outcomes.is_empty() {
        println!("No active transaction journals.");
    } else {
        for outcome in outcomes {
            println!(
                "Recovered transaction {} (completed {} file(s)).",
                outcome.id, outcome.completed_files
            );
        }
    }
    Ok(())
}

fn abandon(args: &[String]) -> Result<()> {
    if args.len() != 4 || args[3] != "--force" {
        bail!("usage: copperpour transaction abandon <project-dir> <transaction-id> --force");
    }
    let project = Path::new(&args[1]);
    let outcome = abandon_file_transaction(project, &args[2]).with_context(|| {
        format!(
            "failed to abandon transaction '{}' in {}",
            args[2],
            project.display()
        )
    })?;
    println!(
        "Abandoned transaction {} without modifying target files.",
        outcome.id
    );
    println!(
        "Evidence retained at {}.",
        outcome.abandoned_journal.display()
    );
    println!("Warning: the abandoned journal contains complete before/after schematic images.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_project_path_rejects_extra_arguments() {
        let args = vec![
            "status".to_owned(),
            "project".to_owned(),
            "extra".to_owned(),
        ];
        assert!(exact_project_path(&args, "status").is_err());
    }

    #[test]
    fn abandon_requires_force() {
        let args = vec![
            "abandon".to_owned(),
            "project".to_owned(),
            "transaction".to_owned(),
        ];
        assert!(abandon(&args).is_err());
    }
}
