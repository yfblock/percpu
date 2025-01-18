use core::sync::atomic::{AtomicBool, Ordering};

static IS_INIT: AtomicBool = AtomicBool::new(false);

const fn align_up_64(val: usize) -> usize {
    const SIZE_64BIT: usize = 0x40;
    (val + SIZE_64BIT - 1) & !(SIZE_64BIT - 1)
}

#[cfg(not(target_os = "none"))]
static PERCPU_AREA_BASE: spin::once::Once<usize> = spin::once::Once::new();

/// Returns the per-CPU data area size for one CPU.
pub fn percpu_area_size() -> usize {
    extern "C" {
        fn _percpu_load_start();
        fn _percpu_load_end();
    }
    // It seems that `_percpu_load_start as usize - _percpu_load_end as usize` will result in more instructions.
    use percpu_macros::percpu_symbol_offset;
    percpu_symbol_offset!(_percpu_load_end) - percpu_symbol_offset!(_percpu_load_start)
}

/// Returns the base address of the per-CPU data area on the given CPU.
///
/// if `cpu_id` is 0, it returns the base address of all per-CPU data areas.
pub fn percpu_area_base(cpu_id: usize) -> usize {
    cfg_if::cfg_if! {
        if #[cfg(target_os = "none")] {
            extern "C" {
                fn _percpu_start();
            }
            let base = _percpu_start as usize;
        } else {
            let base = *PERCPU_AREA_BASE.get().unwrap();
        }
    }
    base + cpu_id * align_up_64(percpu_area_size())
}

/// Initialize the per-CPU data area for `max_cpu_num` CPUs.
pub fn init(max_cpu_num: usize) {
    // avoid re-initialization.
    if IS_INIT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    let size = percpu_area_size();

    #[cfg(target_os = "linux")]
    {
        // we not load the percpu section in ELF, allocate them here.
        let total_size = align_up_64(size) * max_cpu_num;
        let layout = std::alloc::Layout::from_size_align(total_size, 0x1000).unwrap();
        PERCPU_AREA_BASE.call_once(|| unsafe { std::alloc::alloc(layout) as usize });
    }

    let base = percpu_area_base(0);
    for i in 1..max_cpu_num {
        let secondary_base = percpu_area_base(i);
        #[cfg(target_os = "none")]
        {
            extern "C" {
                fn _percpu_end();
            }
            assert!(secondary_base + size <= _percpu_end as usize);
        }
        // copy per-cpu data of the primary CPU to other CPUs.
        unsafe {
            core::ptr::copy_nonoverlapping(base as *const u8, secondary_base as *mut u8, size);
        }
    }
}

/// Reads the architecture-specific per-CPU data register.
///
/// This register is used to hold the per-CPU data base on each CPU.
pub fn read_percpu_reg() -> usize {
    let tp;
    unsafe {
        cfg_if::cfg_if! {
            if #[cfg(target_arch = "x86_64")] {
                tp = if cfg!(target_os = "linux") {
                    SELF_PTR.read_current_raw()
                } else if cfg!(target_os = "none") {
                    x86::msr::rdmsr(x86::msr::IA32_GS_BASE) as usize
                } else {
                    unimplemented!()
                };
            } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
                core::arch::asm!("mv {}, gp", out(reg) tp)
            } else if #[cfg(all(target_arch = "aarch64", not(feature = "arm-el2")))] {
                core::arch::asm!("mrs {}, TPIDR_EL1", out(reg) tp)
            } else if #[cfg(all(target_arch = "aarch64", feature = "arm-el2"))] {
                core::arch::asm!("mrs {}, TPIDR_EL2", out(reg) tp)
            }
        }
    }
    tp
}

/// Writes the architecture-specific per-CPU data register.
///
/// This register is used to hold the per-CPU data base on each CPU.
///
/// # Safety
///
/// This function is unsafe because it writes the low-level register directly.
pub unsafe fn write_percpu_reg(tp: usize) {
    unsafe {
        cfg_if::cfg_if! {
            if #[cfg(target_arch = "x86_64")] {
                if cfg!(target_os = "linux") {
                    const ARCH_SET_GS: u32 = 0x1001;
                    const SYS_ARCH_PRCTL: u32 = 158;
                    core::arch::asm!(
                        "syscall",
                        in("eax") SYS_ARCH_PRCTL,
                        in("edi") ARCH_SET_GS,
                        in("rsi") tp,
                    );
                } else if cfg!(target_os = "none") {
                    x86::msr::wrmsr(x86::msr::IA32_GS_BASE, tp as u64);
                } else {
                    unimplemented!()
                }
                SELF_PTR.write_current_raw(tp);
            } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
                core::arch::asm!("mv gp, {}", in(reg) tp)
            } else if #[cfg(all(target_arch = "aarch64", not(feature = "arm-el2")))] {
                core::arch::asm!("msr TPIDR_EL1, {}", in(reg) tp)
            } else if #[cfg(all(target_arch = "aarch64", feature = "arm-el2"))] {
                core::arch::asm!("msr TPIDR_EL2, {}", in(reg) tp)
            }
        }
    }
}

/// Initializes the per-CPU data register.
///
/// It is equivalent to `write_percpu_reg(percpu_area_base(cpu_id))`, which set
/// the architecture-specific per-CPU data register to the base address of the
/// corresponding per-CPU data area.
///
/// `cpu_id` indicates which per-CPU data area to use.
pub fn init_percpu_reg(cpu_id: usize) {
    let tp = percpu_area_base(cpu_id);
    unsafe { write_percpu_reg(tp) }
}

/// To use `percpu::__priv::NoPreemptGuard::new()` and `percpu::percpu_area_base()` in macro expansion.
#[allow(unused_imports)]
use crate as percpu;

/// On x86, we use `gs:SELF_PTR` to store the address of the per-CPU data area base.
#[cfg(target_arch = "x86_64")]
#[no_mangle]
#[percpu_macros::def_percpu]
static SELF_PTR: usize = 0;
