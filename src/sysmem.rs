// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! The process's resident memory, for the performance overlay.

/// Resident set size of this process in bytes, if the OS tells us.
#[cfg(target_os = "linux")]
pub fn rss_bytes() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096)
}

#[cfg(target_os = "macos")]
pub fn rss_bytes() -> Option<u64> {
    // task_info(mach_task_self(), MACH_TASK_BASIC_INFO, ...)
    #[repr(C)]
    #[derive(Default)]
    struct TimeValue {
        seconds: i32,
        microseconds: i32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct MachTaskBasicInfo {
        virtual_size: u64,
        resident_size: u64,
        resident_size_max: u64,
        user_time: TimeValue,
        system_time: TimeValue,
        policy: i32,
        suspend_count: i32,
    }
    extern "C" {
        static mach_task_self_: u32;
        fn task_info(task: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
    }
    const MACH_TASK_BASIC_INFO: i32 = 20;
    let mut info = MachTaskBasicInfo::default();
    let mut count = (std::mem::size_of::<MachTaskBasicInfo>() / 4) as u32;
    // SAFETY: task_info writes at most `count` 32-bit words into `info`.
    let kr = unsafe { task_info(mach_task_self_, MACH_TASK_BASIC_INFO, &mut info as *mut _ as *mut i32, &mut count) };
    if kr == 0 { Some(info.resident_size) } else { None }
}

/// macOS: what Activity Monitor's "Memory" column shows (phys_footprint), and
/// the internal / compressed / reusable parts, from TASK_VM_INFO.
#[cfg(target_os = "macos")]
pub fn footprint() -> Option<(u64, u64, u64, u64)> {
    extern "C" {
        static mach_task_self_: u32;
        fn task_info(task: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
    }
    const TASK_VM_INFO: i32 = 22;
    // task_vm_info_data_t (pack(4)); words of 64 bits: phys_footprint is the 19th.
    let mut info = [0u64; 64];
    let mut count = (std::mem::size_of_val(&info) / 4) as u32;
    // SAFETY: task_info writes at most `count` 32-bit words into `info`.
    let kr = unsafe { task_info(mach_task_self_, TASK_VM_INFO, info.as_mut_ptr() as *mut i32, &mut count) };
    if kr != 0 || count < 38 {
        return None;
    }
    Some((info[18], info[6], info[15], info[10]))
}

/// macOS (OHB_MEM_DEBUG): memory per VM tag (malloc zones, mimalloc's tag
/// 100, stacks, graphics...): (tag, resident, compressed, dirty, reusable)
/// in bytes, largest first.
#[cfg(target_os = "macos")]
pub fn vm_tags() -> Vec<(u32, u64, u64, u64, u64)> {
    extern "C" {
        static mach_task_self_: u32;
        static vm_page_size: usize;
        fn mach_vm_region_recurse(task: u32, address: *mut u64, size: *mut u64, depth: *mut u32, info: *mut i32, count: *mut u32) -> i32;
    }
    let mut by: std::collections::HashMap<u32, (u64, u64, u64, u64)> = std::collections::HashMap::new();
    // SAFETY: reading a constant exported by the kernel interface.
    let page = unsafe { vm_page_size } as u64;
    let mut addr: u64 = 0;
    let mut depth: u32 = 0;
    for _ in 0..200_000 {
        let mut size: u64 = 0;
        let mut info = [0u32; 19];
        let mut count: u32 = 19;
        // SAFETY: vm_region_submap_info_64 is 19 32-bit words.
        let kr = unsafe { mach_vm_region_recurse(mach_task_self_, &mut addr, &mut size, &mut depth, info.as_mut_ptr() as *mut i32, &mut count) };
        if kr != 0 {
            break;
        }
        // Words: 5 user_tag, 6 pages_resident, 8 pages_swapped_out, 9 pages_dirtied, 12 is_submap, 16 pages_reusable.
        if info[12] != 0 {
            depth += 1;
            continue;
        }
        let e = by.entry(info[5]).or_default();
        e.0 += info[6] as u64 * page;
        e.1 += info[8] as u64 * page;
        e.2 += info[9] as u64 * page;
        e.3 += info[16] as u64 * page;
        addr = addr.saturating_add(size);
    }
    let mut v: Vec<(u32, u64, u64, u64, u64)> = by.into_iter().map(|(t, (r, c, d, u))| (t, r, c, d, u)).collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.2 + x.3));
    v
}

#[cfg(not(target_os = "macos"))]
pub fn vm_tags() -> Vec<(u32, u64, u64, u64, u64)> {
    Vec::new()
}

#[cfg(not(target_os = "macos"))]
pub fn footprint() -> Option<(u64, u64, u64, u64)> {
    None
}

#[cfg(windows)]
pub fn rss_bytes() -> Option<u64> {
    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(process: isize, counters: *mut ProcessMemoryCounters, cb: u32) -> i32;
    }
    let mut c = ProcessMemoryCounters { cb: std::mem::size_of::<ProcessMemoryCounters>() as u32, ..Default::default() };
    // SAFETY: the struct is the documented PROCESS_MEMORY_COUNTERS layout.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    if ok != 0 { Some(c.working_set_size as u64) } else { None }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn rss_bytes() -> Option<u64> {
    None
}
