use std::path::Path;

use anyhow::Context;

use emucap::analysis::{diff, dump, state_diff};

pub fn run(
    dir_a: &Path,
    dir_b: &Path,
    ignore: &[String],
    baseline: Option<&Path>,
    ignore_key: &[String],
    json: bool,
) -> anyhow::Result<()> {
    let a = dump::load(dir_a).with_context(|| format!("덤프 A 로드 실패: {}", dir_a.display()))?;
    let b = dump::load(dir_b).with_context(|| format!("덤프 B 로드 실패: {}", dir_b.display()))?;
    let ignores: Vec<diff::IgnoreSpec> = ignore
        .iter()
        .map(|s| diff::parse_ignore(s))
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("--ignore 파싱 실패: {e}"))?;
    let base = match baseline {
        Some(p) => load_baseline(p)?,
        None => diff::Baseline::empty(),
    };
    // Complete offsets are needed for reusable baselines. Reject over-budget
    // reports instead of silently sampling differences.
    let report =
        diff::diff_bounded(&a, &b, &ignores, &base, 1_000_000).map_err(anyhow::Error::msg)?;

    // 상태(레지스터/DMA/PPU) 디프: 양쪽에 state.json이 있을 때만.
    let sa = dump::load_state_map(dir_a).context("A state.json 로드 실패")?;
    let sb = dump::load_state_map(dir_b).context("B state.json 로드 실패")?;
    let state = match (sa, sb) {
        (Some(sa), Some(sb)) => Some(state_diff::state_diff(&sa, &sb, ignore_key)),
        _ => None,
    };

    if json {
        if let Some(sd) = &state {
            println!(
                "{{\"memory\":{},\"state\":{}}}",
                diff::render_json(&report),
                state_diff::render_json(sd)
            );
        } else {
            println!("{}", diff::render_json(&report));
        }
    } else {
        print!("{}", diff::render_table(&report));
        if let Some(sd) = &state {
            print!("{}", state_diff::render_table(sd));
        }
    }
    Ok(())
}

fn load_baseline(p: &Path) -> anyhow::Result<diff::Baseline> {
    let text = emucap::path_safety::read_bounded_utf8_regular_file_no_follow(p, 32 * 1024 * 1024)
        .with_context(|| format!("기준선 로드 실패: {}", p.display()))?;
    let report: diff::DiffReport = serde_json::from_str(&text).context("기준선 JSON 파싱 실패")?;
    let offsets = report.regions.iter().try_fold(0_usize, |total, region| {
        total.checked_add(region.divergences.len())
    });
    anyhow::ensure!(
        report.regions.len() <= 256 && offsets.is_some_and(|count| count <= 1_000_000),
        "baseline exceeds region or divergence budget"
    );
    Ok(diff::Baseline::from_report(&report))
}

#[cfg(test)]
#[path = "diff_tests.rs"]
mod tests;
