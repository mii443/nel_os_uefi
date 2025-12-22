use core::arch::{asm, x86_64::*};

use alloc::vec;

use crate::info;

pub fn avx_benchmark(use_avx512f: bool) {
    let initial_ticks = crate::time::get_ticks();
    let n = 1_000;

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
    let mut tick = crate::time::get_ticks();
    let mut total_ops = 0;
    while tick - initial_ticks < 5000 {
        if use_avx512f {
            unsafe {
                add_f32_avx512f(a.as_ptr(), b.as_ptr(), out.as_mut_ptr(), n);
            }
        } else {
            unsafe {
                add_f32(a.as_ptr(), b.as_ptr(), out.as_mut_ptr(), n);
            }
        }
        total_ops += n;
        tick = crate::time::get_ticks();
    }

    // Verify results once at the end
    for i in 0..n {
        assert_eq!(out[i], a[i] + b[i]);
    }

    let ops_per_tick = total_ops as f64 / 5000 as f64;
    info!(
        "{} ops/tick with {}.",
        ops_per_tick,
        if use_avx512f {
            "AVX-512F"
        } else {
            "no AVX-512F"
        }
    );
}

#[target_feature(enable = "avx512f")]
unsafe fn add_f32_avx512f(a: *const f32, b: *const f32, out: *mut f32, n: usize) {
    let mut i = 0usize;

    unsafe {
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
