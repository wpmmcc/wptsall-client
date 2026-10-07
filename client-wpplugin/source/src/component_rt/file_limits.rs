use anyhow::{anyhow, Result};

pub(crate) fn bytes_from_megabytes(limit: f64) -> Result<u64> {
    anyhow::ensure!(
        limit.is_finite() && limit >= 0.0,
        "damaged file-size limit; retained"
    );
    if limit == 0.0 {
        return Ok(0);
    }
    let bytes = (limit * (1024.0 * 1024.0)).ceil();
    anyhow::ensure!(
        bytes.is_finite() && bytes < u64::MAX as f64,
        "file-size limit does not fit byte counter; retained"
    );
    Ok(bytes as u64)
}

pub(crate) fn pool_source_budget(limits: impl Iterator<Item = f64>) -> Result<u64> {
    let mut count = 0usize;
    let mut maximum = 0;
    let mut unlimited = false;
    for limit in limits {
        let bytes = bytes_from_megabytes(limit)?;
        count += 1;
        unlimited |= bytes == 0;
        maximum = maximum.max(bytes);
    }
    if count == 0 {
        return Err(anyhow!("credential pool is empty"));
    }
    Ok(if unlimited { 0 } else { maximum })
}

pub(crate) fn strictest(first: u64, second: u64) -> u64 {
    match (first, second) {
        (0, other) | (other, 0) => other,
        _ => first.min(second),
    }
}
