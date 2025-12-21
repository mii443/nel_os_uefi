use core::arch::{asm, x86_64::*};

use alloc::vec;

use crate::info;

pub fn avx_benchmark(use_avx512f: bool) {
    let n = 1_000;
    let iterations = 10_00000;

    // Pre-allocate and initialize arrays once
    let mut a = vec![0f32; n];
    let mut b = vec![0f32; n];
    let mut out = vec![0f32; n];

    for i in 0..n {
        a[i] = i as f32;
        b[i] = (n - i) as f32;
    }

    // Warm-up run
    unsafe {
        add_f32_avx512f(a.as_ptr(), b.as_ptr(), out.as_mut_ptr(), n);
    }

    // Benchmark
    let tick = crate::time::get_ticks();
    for _ in 0..iterations {
        if use_avx512f {
            unsafe {
                add_f32_avx512f(a.as_ptr(), b.as_ptr(), out.as_mut_ptr(), n);
            }
        } else {
            unsafe {
                add_f32(a.as_ptr(), b.as_ptr(), out.as_mut_ptr(), n);
            }
        }
    }
    let elapsed = crate::time::get_ticks() - tick;

    // Verify results once at the end
    for i in 0..n {
        assert_eq!(out[i], a[i] + b[i]);
    }

    let total_ops = n * iterations;
    let ops_per_tick = total_ops as f64 / elapsed as f64;
    info!(
        "AVX512F: {} iterations × {} elements = {} ops in {} ticks ({:.2} Mops/tick) with {}",
        iterations,
        n,
        total_ops,
        elapsed,
        ops_per_tick / 1_000_000.0,
        if use_avx512f { "AVX512F" } else { "scalar" }
    );
}

#[target_feature(enable = "avx512f")]
unsafe fn add_f32_avx512f(a: *const f32, b: *const f32, out: *mut f32, n: usize) {
    let mut i = 0usize;

    while i + 16 <= n {
        let ap = a.add(i);
        let bp = b.add(i);
        let op = out.add(i);

        asm!(
            "vmovups zmm0, [{ap}]",
            "vaddps  zmm0, zmm0, [{bp}]",
            "vmovups [{op}], zmm0",
            ap = in(reg) ap,
            bp = in(reg) bp,
            op = in(reg) op,

            lateout("zmm0") _,

            options(nostack, preserves_flags),
        );

        i += 16;
    }

    while i < n {
        unsafe {
            *out.add(i) = *a.add(i) + *b.add(i);
        }
        i += 1;
    }
}

unsafe fn add_f32(a: *const f32, b: *const f32, out: *mut f32, n: usize) {
    for i in 0..n {
        unsafe {
            *out.add(i) = *a.add(i) + *b.add(i);
        }
    }
}
