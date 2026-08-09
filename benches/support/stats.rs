//! Deterministic sampling and the aggregates the benchmarks report.

/// SplitMix64. Small, seedable and reproducible, which is all the benchmarks
/// need from a generator — they sample queries, they do not simulate.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n`, rejecting the biased tail rather than taking a
    /// modulus over it.
    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0);
        let zone = u64::MAX - (u64::MAX % n) - 1;
        loop {
            let v = self.next_u64();
            if v <= zone {
                return v % n;
            }
        }
    }

    /// Fisher-Yates, so a fixed seed always yields the same sample.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            items.swap(i, j);
        }
    }
}

pub fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f64>() / xs.len() as f64
}

pub fn median(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).expect("timings are never NaN"));
    let mid = v.len() / 2;
    if v.len().is_multiple_of(2) {
        (v[mid - 1] + v[mid]) / 2.0
    } else {
        v[mid]
    }
}

/// Sample standard deviation. Zero for fewer than two observations, where
/// spread is undefined rather than absent.
pub fn stddev(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let m = mean(xs);
    let var = xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (xs.len() - 1) as f64;
    var.sqrt()
}

const BOOTSTRAP_RESAMPLES: usize = 2000;
const BOOTSTRAP_SEED: u64 = 0x5EED_B007;

/// Percentile bootstrap 95% confidence interval for the median.
///
/// The median has no closed-form interval, and resampling makes no normality
/// assumption — which matters here, because process timings are right-skewed
/// by scheduling noise.
pub fn median_ci95(xs: &[f64]) -> (f64, f64) {
    if xs.len() < 2 {
        let m = median(xs);
        return (m, m);
    }
    let mut rng = Rng::new(BOOTSTRAP_SEED);
    let mut medians = Vec::with_capacity(BOOTSTRAP_RESAMPLES);
    let mut draw = vec![0.0; xs.len()];
    for _ in 0..BOOTSTRAP_RESAMPLES {
        for slot in draw.iter_mut() {
            *slot = xs[rng.below(xs.len() as u64) as usize];
        }
        medians.push(median(&draw));
    }
    medians.sort_by(|a, b| a.partial_cmp(b).expect("medians are never NaN"));
    let lo = medians[(BOOTSTRAP_RESAMPLES as f64 * 0.025) as usize];
    let hi = medians[((BOOTSTRAP_RESAMPLES as f64 * 0.975) as usize).min(medians.len() - 1)];
    (lo, hi)
}
