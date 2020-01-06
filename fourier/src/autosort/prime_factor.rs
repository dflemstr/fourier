#![allow(unused_unsafe)]
#![allow(unused_macros)]

use crate::fft::Fft;
use crate::float::FftFloat;
use crate::twiddle::compute_twiddle;
use num_complex::Complex;
use num_traits::One;
use std::cell::Cell;

fn num_factors(factor: usize, mut value: usize) -> (usize, usize) {
    let mut count = 0;
    while value % factor == 0 {
        value /= factor;
        count += 1;
    }
    (count, value)
}

fn extend_twiddles<T: FftFloat>(
    forward_twiddles: &mut Vec<Complex<T>>,
    reverse_twiddles: &mut Vec<Complex<T>>,
    size: usize,
    radix: usize,
    iterations: usize,
) {
    let mut subsize = size;
    for _ in 0..iterations {
        let m = subsize / radix;
        for i in 0..m {
            forward_twiddles.push(Complex::one());
            reverse_twiddles.push(Complex::one());
            for j in 1..radix {
                forward_twiddles.push(compute_twiddle(i * j, subsize, true));
                reverse_twiddles.push(compute_twiddle(i * j, subsize, false));
            }
        }
        subsize /= radix;
    }
}

struct Stages<T> {
    size: usize,
    stages: Vec<(usize, usize)>,
    forward_twiddles: Vec<Complex<T>>,
    reverse_twiddles: Vec<Complex<T>>,
}

impl<T: FftFloat> Stages<T> {
    fn new(size: usize) -> Option<Self> {
        let mut current_size = size;
        let mut stages = Vec::new();
        let mut forward_twiddles = Vec::new();
        let mut reverse_twiddles = Vec::new();

        {
            let (count, new_size) = num_factors(4, current_size);
            if count > 0 {
                stages.push((4, count));
                extend_twiddles(
                    &mut forward_twiddles,
                    &mut reverse_twiddles,
                    current_size,
                    4,
                    count,
                );
            }
            current_size = new_size;
        }
        {
            let (count, new_size) = num_factors(3, current_size);
            if count > 0 {
                stages.push((3, count));
                extend_twiddles(
                    &mut forward_twiddles,
                    &mut reverse_twiddles,
                    current_size,
                    3,
                    count,
                );
            }
            current_size = new_size;
        }
        {
            let (count, new_size) = num_factors(2, current_size);
            if count > 0 {
                stages.push((2, count));
                extend_twiddles(
                    &mut forward_twiddles,
                    &mut reverse_twiddles,
                    current_size,
                    2,
                    count,
                );
            }
            current_size = new_size;
        }
        if current_size != 1 {
            None
        } else {
            Some(Self {
                size,
                stages,
                forward_twiddles,
                reverse_twiddles,
            })
        }
    }
}

/// This macro creates two modules, `radix_f32` and `radix_f64`, containing the radix application
/// functions for each radix.
macro_rules! make_radix_fns {
    {
        @impl $type:ty, $width:ident, $radix:literal, $name:ident, $butterfly:ident
    } => {

        #[multiversion::target_clones("[x86|x86_64]+avx")]
        #[inline]
        pub(super) fn $name(
            input: &[num_complex::Complex<$type>],
            output: &mut [num_complex::Complex<$type>],
            _forward: bool,
            size: usize,
            stride: usize,
            twiddles: &[num_complex::Complex<$type>],
        ) {
            #[target_cfg(target = "[x86|x86_64]+avx")]
            crate::avx_vector! { $type };

            #[target_cfg(not(target = "[x86|x86_64]+avx"))]
            crate::generic_vector! { $type };

            #[target_cfg(target = "[x86|x86_64]+avx")]
            {
                if crate::avx_optimization!($type, $width, $radix, input, output, _forward, size, stride, twiddles) {
                    return
                }
            }

            let get_twiddle = |i, j| unsafe { *twiddles.get_unchecked(j * $radix + i) };
            crate::stage!(
                $width,
                $radix,
                $butterfly,
                input,
                output,
                _forward,
                size,
                stride,
                get_twiddle
            );
        }
    };
    {
        $([$radix:literal, $wide_name:ident, $narrow_name:ident, $butterfly:ident]),*
    } => {
        mod radix_f32 {
        $(
            make_radix_fns! { @impl f32, wide, $radix, $wide_name, $butterfly }
            make_radix_fns! { @impl f32, narrow, $radix, $narrow_name, $butterfly }
        )*
        }
        mod radix_f64 {
        $(
            make_radix_fns! { @impl f64, wide, $radix, $wide_name, $butterfly }
            make_radix_fns! { @impl f64, narrow, $radix, $narrow_name, $butterfly }
        )*
        }
    };
}

make_radix_fns! {
    [2, radix_2_wide, radix_2_narrow, butterfly2],
    [3, radix_3_wide, radix_3_narrow, butterfly3],
    [4, radix_4_wide, radix_4_narrow, butterfly4]
}

/// This macro creates the stage application function.
macro_rules! make_stage_fns {
    { $type:ty, $name:ident, $radix_mod:ident } => {
        #[multiversion::target_clones("[x86|x86_64]+avx")]
        #[inline]
        fn $name(
            input: &mut [Complex<$type>],
            output: &mut [Complex<$type>],
            stages: &Stages<$type>,
            forward: bool,
        ) {
            #[static_dispatch]
            use $radix_mod::radix_2_narrow;
            #[static_dispatch]
            use $radix_mod::radix_2_wide;
            #[static_dispatch]
            use $radix_mod::radix_3_narrow;
            #[static_dispatch]
            use $radix_mod::radix_3_wide;
            #[static_dispatch]
            use $radix_mod::radix_4_narrow;
            #[static_dispatch]
            use $radix_mod::radix_4_wide;

            #[target_cfg(target = "[x86|x86_64]+avx")]
            crate::avx_vector! { $type };

            #[target_cfg(not(target = "[x86|x86_64]+avx"))]
            crate::generic_vector! { $type };

            assert_eq!(input.len(), output.len());
            assert_eq!(stages.size, input.len());

            let mut size = stages.size;
            let mut stride = 1;
            let mut twiddles: &[Complex<$type>] = if forward {
                &stages.forward_twiddles
            } else {
                &stages.reverse_twiddles
            };

            let mut data_in_output = false;
            for (radix, iterations) in &stages.stages {
                let mut iteration = 0;

                // Use partial loads until the stride is large enough
                while stride < width! {} && iteration < *iterations {
                    let (from, to): (&mut _, &mut _) = if data_in_output {
                        (output, input)
                    } else {
                        (input, output)
                    };
                    match radix {
                        4 => radix_4_narrow(from, to, forward, size, stride, twiddles),
                        3 => radix_3_narrow(from, to, forward, size, stride, twiddles),
                        2 => radix_2_narrow(from, to, forward, size, stride, twiddles),
                        _ => unimplemented!("unsupported radix"),
                    }
                    size /= radix;
                    stride *= radix;
                    twiddles = &twiddles[size * radix..];
                    iteration += 1;
                    data_in_output = !data_in_output;
                }

                for _ in iteration..*iterations {
                    let (from, to): (&mut _, &mut _) = if data_in_output {
                        (output, input)
                    } else {
                        (input, output)
                    };
                    match radix {
                        4 => radix_4_wide(from, to, forward, size, stride, twiddles),
                        3 => radix_3_wide(from, to, forward, size, stride, twiddles),
                        2 => radix_2_wide(from, to, forward, size, stride, twiddles),
                        _ => unimplemented!("unsupported radix"),
                    }
                    size /= radix;
                    stride *= radix;
                    twiddles = &twiddles[size * radix..];
                    data_in_output = !data_in_output;
                }
            }
            if forward {
                if data_in_output {
                    input.copy_from_slice(output);
                }
            } else {
                let scale = stages.size as $type;
                if data_in_output {
                    for (x, y) in output.iter().zip(input.iter_mut()) {
                        *y = x / scale;
                    }
                } else {
                    for x in input.iter_mut() {
                        *x /= scale;
                    }
                }
            }
        }
    };
}
make_stage_fns! { f32, apply_stages_f32, radix_f32 }
make_stage_fns! { f64, apply_stages_f64, radix_f64 }

struct PrimeFactor32 {
    stages: Stages<f32>,
    work: Cell<Box<[Complex<f32>]>>,
    size: usize,
}

impl PrimeFactor32 {
    fn new(size: usize) -> Option<Self> {
        if let Some(stages) = Stages::new(size) {
            Some(Self {
                stages,
                work: Cell::new(vec![Complex::default(); size].into_boxed_slice()),
                size,
            })
        } else {
            None
        }
    }
}

impl Fft for PrimeFactor32 {
    type Real = f32;

    fn size(&self) -> usize {
        self.size
    }

    fn transform_in_place(&self, input: &mut [Complex<f32>], forward: bool) {
        let mut work = self.work.take();
        apply_stages_f32(input, &mut work, &self.stages, forward);
        self.work.set(work);
    }
}

pub fn create_f32(size: usize) -> Option<Box<dyn Fft<Real = f32> + Send>> {
    if let Some(fft) = PrimeFactor32::new(size) {
        Some(Box::new(fft))
    } else {
        None
    }
}

struct PrimeFactor64 {
    stages: Stages<f64>,
    work: Cell<Box<[Complex<f64>]>>,
    size: usize,
}

impl PrimeFactor64 {
    fn new(size: usize) -> Option<Self> {
        if let Some(stages) = Stages::new(size) {
            Some(Self {
                stages,
                work: Cell::new(vec![Complex::default(); size].into_boxed_slice()),
                size,
            })
        } else {
            None
        }
    }
}

impl Fft for PrimeFactor64 {
    type Real = f64;

    fn size(&self) -> usize {
        self.size
    }

    fn transform_in_place(&self, input: &mut [Complex<f64>], forward: bool) {
        let mut work = self.work.take();
        apply_stages_f64(input, &mut work, &self.stages, forward);
        self.work.set(work);
    }
}

pub fn create_f64(size: usize) -> Option<Box<dyn Fft<Real = f64> + Send>> {
    if let Some(fft) = PrimeFactor64::new(size) {
        Some(Box::new(fft))
    } else {
        None
    }
}
