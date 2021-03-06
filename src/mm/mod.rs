// Copyright (c) 2017 Colin Finck, RWTH Aachen University
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

pub mod allocator;
pub mod freelist;
mod hole;
#[cfg(test)]
mod test;

use arch;
use arch::mm::paging::{BasePageSize, HugePageSize, LargePageSize, PageSize, PageTableEntryFlags};
use arch::mm::physicalmem::total_memory_size;
#[cfg(feature = "newlib")]
use arch::mm::virtualmem::kernel_heap_end;
use core::mem;
use core::sync::atomic::spin_loop_hint;
use environment;

#[allow(unused)]
/// Physical and virtual address of the first 2 MiB page that maps the kernel.
/// Can be easily accessed through kernel_start_address()
safe_global_var!(static mut KERNEL_START_ADDRESS: usize = 0);
#[allow(unused)]
/// Physical and virtual address of the first page after the kernel.
/// Can be easily accessed through kernel_end_address()
static mut KERNEL_END_ADDRESS: usize = 0; /* CHECK THIS OUT */
#[allow(unused)]
/// Start address of the kernel heap
safe_global_var!(static mut HEAP_START_ADDRESS: usize = 0);
#[allow(unused)]
/// End address of the kernel heap
safe_global_var!(static mut HEAP_END_ADDRESS: usize = 0);
#[allow(unused)]
/// Start address of the user heap
safe_global_var!(static mut USER_HEAP_START_ADDRESS: usize = 0);
#[allow(unused)]
/// End address of the user heap
safe_global_var!(static mut USER_HEAP_END_ADDRESS: usize = 0);
safe_global_var!(static mut USER_HEAP_SIZE: usize = 0);

pub const SAFE_MEM_REGION: u8 = 1;
pub const UNSAFE_MEM_REGION: u8 = 2;
pub const SHARED_MEM_REGION: u8 = 3;
//pub const USER_MEM_REGION: u8 = 10;

pub const UNSAFE_PERMISSION_IN: u32 = 0xC;
pub const UNSAFE_PERMISSION_OUT: u32 = !UNSAFE_PERMISSION_IN;

//pub const USER_PERMISSION_IN: u32 = 0xfC;
//pub const USER_PERMISSION_OUT: u32 = !USER_PERMISSION_IN;

pub fn kernel_start_address() -> usize {
	unsafe { KERNEL_START_ADDRESS }
}

pub fn kernel_end_address() -> usize {
	unsafe { KERNEL_END_ADDRESS }
}

#[cfg(feature = "newlib")]
pub fn task_heap_start() -> usize {
	unsafe { USER_HEAP_START_ADDRESS }
}

#[cfg(feature = "newlib")]
pub fn task_heap_end() -> usize {
	unsafe { USER_HEAP_END_ADDRESS }
}

fn map_heap<S: PageSize>(virt_addr: usize, size: usize, is_kernel: bool) -> usize {
	let mut i: usize = 0;
	let mut flags = PageTableEntryFlags::empty();

	if is_kernel {
		// map the kernel heap
		flags.normal().writable().execute_disable().pkey(UNSAFE_MEM_REGION);
	} else {
		// map the user heap
		flags.normal().writable().execute_disable();
	}
	while i < align_down!(size, S::SIZE) {
		match arch::mm::physicalmem::allocate_aligned(S::SIZE, S::SIZE) {
			Ok(phys_addr) => {
				arch::mm::paging::map::<S>(virt_addr + i, phys_addr, 1, flags);
                i += S::SIZE;
			}
			Err(_) => {
				error!("Unable to allocate page frame of size 0x{:x}", S::SIZE);
				return i;
			}
		}
	}

	i
}

#[cfg(not(test))]
pub fn init() {
	// Calculate the start and end addresses of the 2 MiB page(s) that map the kernel.
	unsafe {
		KERNEL_START_ADDRESS = align_down!(
			environment::get_base_address(),
			arch::mm::paging::LargePageSize::SIZE
		);
		KERNEL_END_ADDRESS = align_up!(
			environment::get_base_address() + environment::get_image_size(),
			arch::mm::paging::LargePageSize::SIZE
		);
		info!("KERNEL_START_ADDRESS: {:#X}", KERNEL_START_ADDRESS);
		info!("get_base_address: {:#X}", environment::get_base_address());
		info!("get_image_size: {:#X}", environment::get_image_size());
	}

	arch::mm::init();
	arch::mm::init_page_tables();
	// Init the first pages for BOOT_INFO, Multiboot, SMP info, and so on. 
	init_pages_before_kernel();

	info!("Total memory size: {} MB", total_memory_size() >> 20);

	// we reserve physical memory for the required page tables
	// In worst case, we use page size of BasePageSize::SIZE
	let npages = total_memory_size() / BasePageSize::SIZE;
	let npage_3tables = npages / (BasePageSize::SIZE / mem::align_of::<usize>()) + 1;
	let npage_2tables = npage_3tables / (BasePageSize::SIZE / mem::align_of::<usize>()) + 1;
	let npage_1tables = npage_2tables / (BasePageSize::SIZE / mem::align_of::<usize>()) + 1;
	let reserved_space =
		(npage_3tables + npage_2tables + npage_1tables) * BasePageSize::SIZE + LargePageSize::SIZE;
	let has_1gib_pages = arch::processor::supports_1gib_pages();

	//info!("reserved space {} KB", reserved_space >> 10);
	info!("reserved space {:#X}", reserved_space);

	if total_memory_size() < kernel_end_address() + reserved_space + LargePageSize::SIZE {
		error!("No enough memory available!");

		loop {
			spin_loop_hint();
		}
	}

	/* Init  .safe_data section */
	allocate_safe_data();
	/* Init  .unsafe_data section */
	allocate_unsafe_data();

	let mut map_addr: usize;
	let mut map_size: usize;

	#[cfg(feature = "newlib")]
	{
		info!("An application with a C-based runtime is running on top of HermitCore!");

		let size = 2 * LargePageSize::SIZE;
		let start = allocate(size, true);
		unsafe {
			::ALLOCATOR.init(start, size);
		}

		info!("Kernel heap size: {} MB", size >> 20);
		let user_heap_size = align_down!(
			total_memory_size() - kernel_end_address() - reserved_space - 3 * LargePageSize::SIZE,
			LargePageSize::SIZE
		);

		map_addr = kernel_heap_end();
		map_size = user_heap_size + size;
		unsafe {
                        HEAP_START_ADDRESS = map_addr;
                        USER_HEAP_START_ADDRESS = HEAP_START_ADDRESS + size;
                        USER_HEAP_SIZE = user_heap_size;
                        USER_HEAP_END_ADDRESS = USER_HEAP_START_ADDRESS + USER_HEAP_SIZE; 

                        // map heap
                        let counter = map_heap::<LargePageSize>(map_addr, map_size, false);
                        map_size -= counter;
                        map_addr += counter;

                        // remap kernel heap
                        for i in 0..size/LargePageSize::SIZE {
                                let mut flags = PageTableEntryFlags::empty();
                                flags.normal().writable().execute_disable().pkey(UNSAFE_MEM_REGION);
                                let physical_addr = align_down!(arch::mm::paging::virtual_to_physical(HEAP_START_ADDRESS +  i*LargePageSize::SIZE), LargePageSize::SIZE);
                                arch::mm::paging::map::<LargePageSize>(HEAP_START_ADDRESS +  i*LargePageSize::SIZE, physical_addr, 1, flags);
                        }
                }
	}

	#[cfg(not(feature = "newlib"))]
	{
		info!("A pure Rust application is running on top of HermitCore!");

		// At first, we map only a small part into the heap.
		// Afterwards, we already use the heap and map the rest into
		// the virtual address space.

		let virt_size: usize = 4*LargePageSize::SIZE; // kernel heap is 8MB
		unsafe {
			USER_HEAP_SIZE = align_down!(
				total_memory_size() - kernel_end_address() - reserved_space,
				LargePageSize::SIZE
			) - virt_size;
		}

		let virt_addr = if has_1gib_pages && virt_size > HugePageSize::SIZE {
			arch::mm::virtualmem::allocate_aligned(
				align_up!(virt_size, HugePageSize::SIZE),
				HugePageSize::SIZE,
			)
			.unwrap()
		} else {
			arch::mm::virtualmem::allocate_aligned(virt_size, LargePageSize::SIZE).unwrap()
		};

		info!(
			"Kernel Heap: size {} MB, start address 0x{:x}",
			virt_size >> 20,
			virt_addr
		);

		// try to map a huge page
		let mut counter = if has_1gib_pages && virt_size > HugePageSize::SIZE {
			map_heap::<HugePageSize>(virt_addr, HugePageSize::SIZE, true)
		} else {
			0
		};

		if counter == 0 {
			// fall back to large pages
			counter = map_heap::<LargePageSize>(virt_addr, LargePageSize::SIZE, true);
		}

		unsafe {
			HEAP_START_ADDRESS = virt_addr;
			// init the kernel heap
			::ALLOCATOR.init(virt_addr, virt_size);
		}

		map_addr = virt_addr + counter;
		map_size = virt_size - counter;

                if has_1gib_pages
		    && map_size > HugePageSize::SIZE
	            && (map_addr & !(HugePageSize::SIZE - 1)) == 0
	        {
            	        let counter = map_heap::<HugePageSize>(map_addr, map_size, true);
		        map_size -= counter;
		        map_addr += counter;
                }

	        if map_size > LargePageSize::SIZE {
		        let counter = map_heap::<LargePageSize>(map_addr, map_size, true);
		        map_size -= counter;
		        map_addr += counter;
	        }
        }

	unsafe {
		HEAP_END_ADDRESS = map_addr;

		info!(
			"Kernel Heap is located at 0x{:x} -- 0x{:x} ({} Bytes unmapped)",
			HEAP_START_ADDRESS, HEAP_END_ADDRESS, map_size
		);
	}
}

pub fn init_user_allocator() {
        #[cfg(not(feature = "newlib"))]
        {
		// User Heap Initialization
		let user_heap_size: usize = unsafe {USER_HEAP_SIZE};
		let user_heap_start_addr = arch::mm::virtualmem::allocate_aligned(user_heap_size, LargePageSize::SIZE).unwrap();
		// Map user heap
		let map_count = map_heap::<LargePageSize>(user_heap_start_addr, user_heap_size, false);
		if map_count != user_heap_size {
			panic!("User Heap Map fails!!");
		}

		unsafe {
			USER_HEAP_START_ADDRESS = user_heap_start_addr;
			USER_HEAP_END_ADDRESS = user_heap_start_addr + user_heap_size;
			::ALLOCATOR.init(user_heap_start_addr, user_heap_size);
		}
        }
}
pub fn print_information() {
	arch::mm::physicalmem::print_information();
	arch::mm::virtualmem::print_information();
}

pub fn allocate_iomem(sz: usize) -> usize {
	let size = align_up!(sz, BasePageSize::SIZE);

	let physical_address = arch::mm::physicalmem::allocate(size).unwrap();
	let virtual_address = arch::mm::virtualmem::allocate(size).unwrap();

	let count = size / BasePageSize::SIZE;
	let mut flags = PageTableEntryFlags::empty();
	flags.normal().writable().execute_disable();
	arch::mm::paging::map::<BasePageSize>(virtual_address, physical_address, count, flags);

	virtual_address
}

fn init_pages_before_kernel()
{
	let virtual_address = 0x0usize;
	let physical_address = 0x0usize;
	let count = 0x200000usize / BasePageSize::SIZE;
	let mut flags = PageTableEntryFlags::empty();
	flags.normal().writable().execute_disable().pkey(SAFE_MEM_REGION);
	arch::mm::paging::map::<BasePageSize>(virtual_address, physical_address, count, flags);

	/* The first 4kb page is used by user (as a null pointer) */
	arch::mm::paging::set_pkey_on_page_table_entry::<BasePageSize>(0x0usize, 1, 0x00u8);
}

pub fn allocate(sz: usize, execute_disable: bool) -> usize {
	let size = align_up!(sz, BasePageSize::SIZE);

	let physical_address = arch::mm::physicalmem::allocate(size).unwrap();
	let virtual_address = arch::mm::virtualmem::allocate(size).unwrap();

	let count = size / BasePageSize::SIZE;
	let mut flags = PageTableEntryFlags::empty();
	flags.normal().writable().pkey(SAFE_MEM_REGION);
	if execute_disable {
		flags.execute_disable();
	}
	arch::mm::paging::map::<BasePageSize>(virtual_address, physical_address, count, flags);

	virtual_address
}

pub fn unsafe_allocate(sz: usize, execute_disable: bool) -> usize {
	let size = align_up!(sz, BasePageSize::SIZE);

	let physical_address = arch::mm::physicalmem::allocate_aligned(size, BasePageSize::SIZE).unwrap();
	let virtual_address = arch::mm::virtualmem::allocate_aligned(size, BasePageSize::SIZE).unwrap();

	let count = size / BasePageSize::SIZE;
	let mut flags = PageTableEntryFlags::empty();
	flags.normal().writable().pkey(UNSAFE_MEM_REGION);
	if execute_disable {
		flags.execute_disable();
	}
	arch::mm::paging::map::<BasePageSize>(virtual_address, physical_address, count, flags);

	virtual_address
}

pub fn shared_allocate(sz: usize, execute_disable: bool) -> usize {
	let size = align_up!(sz, BasePageSize::SIZE);

	let physical_address = arch::mm::physicalmem::allocate_aligned(size, BasePageSize::SIZE).unwrap();
	let virtual_address = arch::mm::virtualmem::allocate_aligned(size, BasePageSize::SIZE).unwrap();

	let count = size / BasePageSize::SIZE;
	let mut flags = PageTableEntryFlags::empty();
	flags.normal().writable().pkey(SHARED_MEM_REGION);
	if execute_disable {
		flags.execute_disable();
	}
	arch::mm::paging::map::<BasePageSize>(virtual_address, physical_address, count, flags);

	virtual_address
}

pub fn user_allocate(sz: usize, execute_disable: bool) -> usize {
	let size = align_up!(sz, BasePageSize::SIZE);

	let physical_address = arch::mm::physicalmem::allocate_aligned(size, BasePageSize::SIZE).unwrap();
	let virtual_address = arch::mm::virtualmem::allocate_aligned(size, BasePageSize::SIZE).unwrap();

	let count = size / BasePageSize::SIZE;
	let mut flags = PageTableEntryFlags::empty();
	flags.normal().writable();
	if execute_disable {
		flags.execute_disable();
	}
	arch::mm::paging::map::<BasePageSize>(virtual_address, physical_address, count, flags);

	virtual_address
}

fn allocate_safe_data() {
    let safe_data_start = 0x400000usize;
	let aligned_size = 0x200000usize;
	/* We harcode the physical address here */
	let physical_address = 0x400000usize;
	//let physical_address = arch::mm::physicalmem::allocate_aligned(aligned_size, LargePageSize::SIZE).unwrap();
	let count = aligned_size / LargePageSize::SIZE;
	let mut flags = PageTableEntryFlags::empty();
	flags.normal().writable().pkey(SAFE_MEM_REGION);
	flags.execute_disable();
	arch::mm::paging::map::<LargePageSize>(safe_data_start, physical_address, count, flags);
	info!("safe .data starts at (virt_address: {:#X}, phys_address: {:#X}), size: {:#X}", safe_data_start, physical_address, aligned_size);
}

fn allocate_unsafe_data() {
    let unsafe_data_start = 0x600000usize;
	let aligned_size = 0x200000usize;
	/* We harcode the physical address here */
	let physical_address = 0x600000usize;
	let count = aligned_size / LargePageSize::SIZE;
	let mut flags = PageTableEntryFlags::empty();
	flags.normal().writable().pkey(UNSAFE_MEM_REGION);
	flags.execute_disable();
	arch::mm::paging::map::<LargePageSize>(unsafe_data_start, physical_address, count, flags);
	info!("unsafe .data starts at (virt_address: {:#X}, phys_address: {:#X}), size: {:#X}", unsafe_data_start, physical_address, aligned_size);
}

pub fn deallocate(virtual_address: usize, sz: usize) {
	let size = align_up!(sz, BasePageSize::SIZE);

	if let Some(entry) = arch::mm::paging::get_page_table_entry::<BasePageSize>(virtual_address) {
		arch::mm::virtualmem::deallocate(virtual_address, size);
		arch::mm::physicalmem::deallocate(entry.address(), size);
	} else {
		panic!(
			"No page table entry for virtual address {:#X}",
			virtual_address
		);
	}
}
