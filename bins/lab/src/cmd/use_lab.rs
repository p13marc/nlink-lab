//! `nlink-lab use`: the current lab, which lab-scoped commands act on
//! when `-L` does not name one.

use crate::ctx::{Ctx, LabFrom, caller_uid, resolve_lab};
use crate::output::{Report, emit};

#[derive(clap::Args)]
pub struct Args {
    /// The lab to make current. Without it, show the current lab and
    /// where it comes from.
    #[arg(add = crate::ctx::lab_completer(), conflicts_with = "clear")]
    pub lab: Option<String>,

    /// Forget the current lab.
    #[arg(long)]
    pub clear: bool,
}

/// `use`'s result: the lab commands act on, and why.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct UseReport {
    pub lab: Option<String>,
    /// `flag`, `env` (`NLINK_LAB`), `context` (`nlink-lab use`), `only`
    /// (the only running lab).
    pub from: Option<LabFrom>,
    /// Why there is none, or what overrides the one just set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Report for UseReport {
    fn text(&self) -> String {
        let mut out = match (&self.lab, self.from) {
            (Some(lab), Some(LabFrom::Context)) => format!("{lab} (set by `nlink-lab use`)\n"),
            (Some(lab), Some(LabFrom::Env)) => format!("{lab} (from NLINK_LAB)\n"),
            (Some(lab), Some(LabFrom::Only)) => format!("{lab} (the only running lab)\n"),
            (Some(lab), _) => format!("{lab}\n"),
            (None, _) => "no current lab\n".to_string(),
        };
        if let Some(note) = &self.note {
            out.push_str(&format!("  {note}\n"));
        }
        out
    }

    fn quiet(&self) -> Option<String> {
        Some(
            self.lab
                .as_ref()
                .map(|l| format!("{l}\n"))
                .unwrap_or_default(),
        )
    }
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let uid = caller_uid();
    let overridden = std::env::var("NLINK_LAB")
        .ok()
        .filter(|l| !l.is_empty())
        .map(|l| format!("NLINK_LAB={l} overrides it in this shell"));
    let report = if args.clear {
        nlink_lab::internal::state::context_set(uid, None)?;
        UseReport {
            lab: None,
            from: None,
            note: None,
        }
    } else if let Some(lab) = args.lab {
        nlink_lab::internal::state::validate_lab_name(&lab)?;
        if !nlink_lab::internal::state::exists(&lab) {
            return Err(nlink_lab::Error::NotFound { name: lab });
        }
        nlink_lab::internal::state::context_set(uid, Some(&lab))?;
        UseReport {
            lab: Some(lab),
            from: Some(LabFrom::Context),
            note: overridden,
        }
    } else {
        match resolve_lab(None) {
            Ok((lab, from)) => UseReport {
                lab: Some(lab),
                from: Some(from),
                note: None,
            },
            Err(e) => UseReport {
                lab: None,
                from: None,
                note: Some(e.to_string()),
            },
        }
    };
    emit(ctx, &report)
}
