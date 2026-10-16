//! 输出向量的数值检查与 L2 归一化。
//! 每个向量都检查：无 NaN / Inf，归一化前的范数有限且非零，归一化后的范数接近 1；不合格就报错，不入库。
//! 与参考实现 `scripts/golden/llama_embed.py` 相同：用 f64 求范数和相除，再转回 f32。
//! 粗排向量的截断与重新归一化（`truncate`）也在这里：核心算好后交给索引，存储时的量化在 `rummage_index::vindex::quantize`。

use crate::Error;

/// 归一化后范数允许偏离 1 的量。f32 舍入误差远小于此值，超出说明数值已经出问题。
const NORM_TOLERANCE: f64 = 1e-3;

fn norm(v: &[f32]) -> f64 {
    v.iter()
        .map(|&x| f64::from(x) * f64::from(x))
        .sum::<f64>()
        .sqrt()
}

pub fn normalize(raw: Vec<f32>) -> Result<Vec<f32>, Error> {
    if raw.is_empty() {
        return Err(Error::BadVector("向量为空。".to_string()));
    }
    if raw.iter().any(|x| !x.is_finite()) {
        return Err(Error::BadVector("向量含有 NaN 或 Inf。".to_string()));
    }
    let n = norm(&raw);
    if !n.is_finite() || n == 0.0 {
        return Err(Error::BadVector(format!("归一化前的范数为 {n}。")));
    }
    let out: Vec<f32> = raw.iter().map(|&x| (f64::from(x) / n) as f32).collect();
    let after = norm(&out);
    if (after - 1.0).abs() > NORM_TOLERANCE {
        return Err(Error::BadVector(format!("归一化后的范数为 {after}。")));
    }
    Ok(out)
}

/// 粗排向量的维数
pub const COARSE_DIM: usize = 256;

/// 粗排向量：截断到前 `COARSE_DIM` 维（不足的不补）并重新 L2 归一化。用 f32 求范数和相除，
/// 与已写入索引的粗排数据一致。范数不是正数（全零向量）时为全零
pub fn truncate(v: &[f32]) -> Vec<f32> {
    let head = &v[..v.len().min(COARSE_DIM)];
    let norm = head.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        head.iter().map(|x| x / norm).collect()
    } else {
        vec![0.0; head.len()]
    }
}

/// 余弦相似度；任一向量范数为零时结果为 NaN。
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| f64::from(x) * f64::from(y))
        .sum();
    dot / (norm(a) * norm(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        let v = normalize(vec![3.0, 4.0]).unwrap();
        assert_eq!(v, vec![0.6, 0.8]);
        // 极小但非零的范数也能归一化。
        let v = normalize(vec![1e-30, 0.0]).unwrap();
        assert_eq!(v, vec![1.0, 0.0]);
    }

    #[test]
    fn rejects_bad_vectors() {
        assert!(normalize(vec![]).is_err());
        assert!(normalize(vec![0.0, 0.0]).is_err());
        assert!(normalize(vec![1.0, f32::NAN]).is_err());
        assert!(normalize(vec![f32::INFINITY, 1.0]).is_err());
        // 范数在 f64 中计算：f32 的极大值和次正规数都不会溢出或下溢。
        assert!(normalize(vec![f32::MAX, f32::MAX]).is_ok());
        assert!(normalize(vec![f32::from_bits(1); 4]).is_ok());
    }

    #[test]
    fn cosine_values() {
        assert!((cosine(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-12);
        assert!(cosine(&[1.0, 0.0], &[0.0, 3.0]).abs() < 1e-12);
        assert!((cosine(&[1.0, 1.0], &[-1.0, -1.0]) + 1.0).abs() < 1e-12);
        assert!(cosine(&[0.0, 0.0], &[1.0, 0.0]).is_nan());
    }
}
