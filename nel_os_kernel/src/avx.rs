use core::arch::{asm, x86_64::*};

use alloc::vec;

use crate::info;

pub fn avx_benchmark(use_avx512f: bool) {
    let initial_ticks = crate::time::get_ticks();
    let n = 5_000;

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
        add_f32(a.as_ptr(), b.as_ptr(), out.as_mut_ptr(), n);
    }

    // Benchmark
    let mut tick = crate::time::get_ticks();
    let mut total_ops = 0;
    while tick - initial_ticks < 1000 {
        if use_avx512f {
            unsafe {
                add_f32_avx512f(a.as_ptr(), b.as_ptr(), out.as_mut_ptr(), n);
                //add_f32(a.as_ptr(), b.as_ptr(), out.as_mut_ptr(), n);
            }
        } else {
            unsafe {
                add_f32_x87(a.as_ptr(), b.as_ptr(), out.as_mut_ptr(), n);
            }
        }
        total_ops += 1;
        tick = crate::time::get_ticks();
    }

    // Verify results once at the end
    for i in 0..n {
        assert_eq!(out[i], a[i] + b[i]);
    }

    info!(
        "{} ops/s with {}.",
        total_ops,
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

#[target_feature(enable = "avx512f")]
#[inline(never)]
#[unsafe(no_mangle)]
unsafe fn add_f32(a: *const f32, b: *const f32, out: *mut f32, n: usize) {
    for i in 0..n {
        unsafe {
            *out.add(i) = *a.add(i) + *b.add(i);
        }
    }
}

#[inline(never)]
#[unsafe(no_mangle)]
unsafe fn add_f32_woavx512f(a: *const f32, b: *const f32, out: *mut f32, n: usize) {
    for i in 0..n {
        unsafe {
            *out.add(i) = *a.add(i) + *b.add(i);
        }
    }
}

#[inline(never)]
#[unsafe(no_mangle)]
unsafe fn add_f32_x87(a: *const f32, b: *const f32, out: *mut f32, n: usize) {
    let mut i = 0;
    while i < n {
        let ap = a.add(i);
        let bp = b.add(i);
        let op = out.add(i);

        asm!(
            "fld dword ptr [{ap}]",
            "fadd dword ptr [{bp}]",
            "fstp dword ptr [{op}]",
            ap = in(reg) ap,
            bp = in(reg) bp,
            op = in(reg) op,
            options(nostack, preserves_flags)
        );

        i += 1;
    }
}
