// Copyright (c) 2017 Colin Finck, RWTH Aachen University
//
// MIT License
//
// Permission is hereby granted, free of charge, to any person obtaining
// a copy of this software and associated documentation files (the
// "Software"), to deal in the Software without restriction, including
// without limitation the rights to use, copy, modify, merge, publish,
// distribute, sublicense, and/or sell copies of the Software, and to
// permit persons to whom the Software is furnished to do so, subject to
// the following conditions:
//
// The above copyright notice and this permission notice shall be
// included in all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
// EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
// NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
// LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
// WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use arch::x86_64::irq::*;
use arch::x86_64::percore::*;
use arch::x86_64::processor;
use core::fmt;
use core::marker::PhantomData;
use logging::*;
use synch::spinlock::*;
use tasks::*;
use x86::shared::*;


extern "C" {
	#[link_section = ".percore"]
	static __core_id: u32;

	#[link_section = ".percore"]
	static current_task: *const task_t;

	#[linkage = "extern_weak"]
	static runtime_osinit: *const u8;

	static cmdline: *const u8;
	static cmdsize: usize;
	static image_size: usize;
	static kernel_start: u8;

	fn apic_eoi(int_no: usize);
	fn get_pages(npages: usize) -> usize;
	fn get_zeroed_page() -> usize;
	fn ipi_tlb_flush() -> i32;
	fn irq_install_handler(irq: u32, handler: unsafe extern "C" fn(s: *const state)) -> i32;
}

lazy_static! {
	static ref ROOT_PAGETABLE: SpinlockIrqSave<&'static mut PageTable<PML4>> =
		SpinlockIrqSave::new(unsafe { &mut *(0xFFFF_FFFF_FFFF_F000 as *mut PageTable<PML4>) });
}


/// Number of Offset bits of a virtual address for a 4KiB page, which are shifted away to get its Page Frame Number (PFN).
const PAGE_BITS: usize = 12;

/// Number of bits of the index in each table (PML4, PDPT, PDT, PGT).
const PAGE_MAP_BITS: usize = 9;

/// A mask where PAGE_MAP_BITS are set to calculate a table index.
const PAGE_MAP_MASK: usize = 0x1FF;


bitflags! {
	/// Possible flags for an entry in either table (PML4, PDPT, PDT, PGT)
	///
	/// See Intel Vol. 3A, Tables 4-14 through 4-19
	struct PageTableEntryFlags: usize {
		/// Set if this entry is valid and points to a page or table.
		const PRESENT = 1 << 0;

		/// Set if memory referenced by this entry shall be writable.
		const WRITABLE = 1 << 1;

		/// Set if memory referenced by this entry shall be accessible from user-mode (Ring 3).
		const USER_ACCESSIBLE = 1 << 2;

		/// Set if Write-Through caching shall be enabled for memory referenced by this entry.
		/// Otherwise, Write-Back caching is used.
		const WRITE_THROUGH = 1 << 3;

		/// Set if caching shall be disabled for memory referenced by this entry.
		const CACHE_DISABLE = 1 << 4;

		/// Set if software has accessed this entry (for memory access or address translation).
		const ACCESSED = 1 << 5;

		/// Only for page entries: Set if software has written to the memory referenced by this entry.
		const DIRTY = 1 << 6;

		/// Only for page entries in PDPT or PDT: Set if this entry references a 1GiB (PDPT) or 2MiB (PDT) page.
		const HUGE_PAGE = 1 << 7;

		/// Only for page entries: Set if this address translation is global for all tasks and does not need to
		/// be flushed from the TLB when CR3 is reset.
		const GLOBAL = 1 << 8;

		/// Set if code execution shall be disabled for memory referenced by this entry.
		const EXECUTE_DISABLE = 1 << 63;
    }
}

impl PageTableEntryFlags {
	/// An empty set of flags for unused/zeroed table entries.
	/// Needed as long as empty() is no const function.
	const BLANK: PageTableEntryFlags = PageTableEntryFlags { bits: 0 };
}

/// An entry in either table (PML4, PDPT, PDT, PGT)
#[derive(Clone, Copy)]
struct PageTableEntry {
	/// Physical memory address this entry refers, combined with flags from PageTableEntryFlags.
	physical_address_and_flags: usize
}

impl PageTableEntry {
	/// Return the stored physical address.
	fn address(&self) -> usize {
		self.physical_address_and_flags & !(BasePageSize::SIZE - 1) & !(PageTableEntryFlags::EXECUTE_DISABLE).bits()
	}

	/// Zero this entry to mark it as unused.
	fn zero(&mut self) {
		self.physical_address_and_flags = 0;
	}

	/// Returns whether this entry is valid (present).
	fn is_present(&self) -> bool {
		(self.physical_address_and_flags & PageTableEntryFlags::PRESENT.bits()) != 0
	}

	/// Mark this as a valid (present) entry and set address translation and flags.
	///
	/// # Arguments
	///
	/// * `physical_address` - The physical memory address this entry shall translate to
	/// * `flags` - Flags from PageTableEntryFlags (note that the PRESENT and ACCESSED flags are set automatically)
	fn set(&mut self, physical_address: usize, flags: PageTableEntryFlags) {
		if flags.contains(PageTableEntryFlags::HUGE_PAGE) {
			// HUGE_PAGE may indicate a 2MiB or 1GiB page.
			// We don't know this here, so we can only verify that at least the offset bits for a 2MiB page are zero.
			assert!(physical_address & (LargePageSize::SIZE - 1) == 0, "Physical address not on 2MiB page boundary (physical_address = {:#X})", physical_address);
		} else {
			// Verify that the offset bits for a 4KiB page are zero.
			assert!(physical_address & (BasePageSize::SIZE - 1) == 0, "Physical address not on 4KiB page boundary (physical_address = {:#X})", physical_address);
		}

		// Verify that the physical address does not exceed the CPU's physical address width.
		assert!(physical_address >> processor::get_physical_address_bits() == 0, "Physical address exceeds CPU's physical address width (physical_address = {:#X})", physical_address);

		self.physical_address_and_flags = physical_address | (PageTableEntryFlags::PRESENT | PageTableEntryFlags::ACCESSED | flags).bits();
	}
}

/// A generic interface to support all possible page sizes.
///
/// This is defined as a subtrait of Copy to enable #[derive(Clone, Copy)] for Page.
/// Currently, deriving implementations for these traits only works if all dependent types implement it as well.
trait PageSize: Copy {
	/// The page size in bytes.
	const SIZE: usize;

	/// The page table level at which a page of this size is mapped (from 0 for PGT through 3 for PML4).
	/// Implemented as a numeric value to enable numeric comparisons.
	const MAP_LEVEL: usize;

	/// Any extra flag that needs to be set to map a page of this size.
	/// For example: PageTableEntryFlags::HUGE_PAGE
	const MAP_EXTRA_FLAG: PageTableEntryFlags;
}

/// A 4KiB page mapped in the PGT.
#[derive(Clone, Copy)]
enum BasePageSize {}
impl PageSize for BasePageSize {
	const SIZE: usize = 4096;
	const MAP_LEVEL: usize = 0;
	const MAP_EXTRA_FLAG: PageTableEntryFlags = PageTableEntryFlags::BLANK;
}

/// A 2MiB page mapped in the PDT.
#[derive(Clone, Copy)]
enum LargePageSize {}
impl PageSize for LargePageSize {
	const SIZE: usize = 2 * 1024 * 1024;
	const MAP_LEVEL: usize = 1;
	const MAP_EXTRA_FLAG: PageTableEntryFlags = PageTableEntryFlags::HUGE_PAGE;
}

/// A 1GiB page mapped in the PDPT.
#[derive(Clone, Copy)]
enum HugePageSize {}
impl PageSize for HugePageSize {
	const SIZE: usize = 1024 * 1024 * 1024;
	const MAP_LEVEL: usize = 2;
	const MAP_EXTRA_FLAG: PageTableEntryFlags = PageTableEntryFlags::HUGE_PAGE;
}

/// A memory page of the size given by S.
#[derive(Clone, Copy)]
struct Page<S: PageSize> {
	/// Virtual memory address of this page.
	/// This is rounded to a page size boundary on creation.
	virtual_address: usize,

	/// Required by Rust to support the S parameter.
	size: PhantomData<S>,
}

impl<S: PageSize> Page<S> {
	/// Flushes this page from the TLB of this CPU.
	unsafe fn flush_from_tlb(&self) {
		asm!("invlpg ($0)" :: "r"(self.virtual_address) : "memory");
	}

	/// Returns whether the given virtual address is a valid one in the x86-64 memory model.
	///
	/// Current x86-64 supports only 48-bit for virtual memory addresses.
	/// This is enforced by requiring bits 63 through 48 to replicate bit 47 (cf. Intel Vol. 1, 3.3.7.1).
	/// As a consequence, the address space is divided into the two valid regions 0x8000_0000_0000
	/// and 0xFFFF_8000_0000_0000.
	///
	/// Although we could make this check depend on the actual linear address width from the CPU,
	/// any extension above 48-bit would require a new page table level, which we don't implement.
	fn is_valid_address(virtual_address: usize) -> bool {
		(virtual_address < 0x8000_0000_0000 || virtual_address >= 0xFFFF_8000_0000_0000)
	}

	/// Returns a Page including the given virtual address.
	/// That means, the address is rounded down to a page size boundary.
	fn including_address(virtual_address: usize) -> Self {
		assert!(Self::is_valid_address(virtual_address));

		if S::SIZE == 1024 * 1024 * 1024 {
			assert!(processor::supports_1gib_pages());
		}

		Self {
			virtual_address: virtual_address & !(S::SIZE - 1),
			size: PhantomData,
		}
	}

	/// Returns a PageIter to iterate from the given first Page to the given last Page (inclusive).
	fn range(first: Self, last: Self) -> PageIter<S> {
		assert!(first.virtual_address <= last.virtual_address);
		PageIter { current: first, last: last }
	}

	/// Returns the index of this page in the table given by L.
	fn table_index<L: PageTableLevel>(&self) -> usize {
		assert!(L::LEVEL >= S::MAP_LEVEL);
		self.virtual_address >> PAGE_BITS >> L::LEVEL * PAGE_MAP_BITS & PAGE_MAP_MASK
	}
}

/// An iterator to walk through a range of pages of size S.
struct PageIter<S: PageSize> {
	current: Page<S>,
	last: Page<S>,
}

impl<S: PageSize> Iterator for PageIter<S> {
	type Item = Page<S>;

	fn next(&mut self) -> Option<Page<S>> {
		if self.current.virtual_address <= self.last.virtual_address {
			let p = self.current;
			self.current.virtual_address += S::SIZE;
			Some(p)
		} else {
			None
		}
	}
}

/// An interface to allow for a generic implementation of struct PageTable for all 4 page tables.
/// Must be implemented by all page tables.
trait PageTableLevel {
	/// Numeric page table level (from 0 for PGT through 3 for PML4) to enable numeric comparisons.
	const LEVEL: usize;
}

/// An interface for page tables with sub page tables (all except PGT).
/// Having both PageTableLevel and PageTableLevelWithSubtables leverages Rust's typing system to provide
/// a next_table_for_page method only for those that have sub page tables.
///
/// Kudos to Philipp Oppermann for the trick!
trait PageTableLevelWithSubtables: PageTableLevel {
	type SubtableLevel;
}

/// The Page Map Level 4 (PML4) table, with numeric level 3 and PDPT subtables.
enum PML4 {}
impl PageTableLevel for PML4 {
	const LEVEL: usize = 3;
}

impl PageTableLevelWithSubtables for PML4 {
	type SubtableLevel = PDPT;
}

/// A Page Directory Pointer Table (PDPT), with numeric level 2 and PDT subtables.
enum PDPT {}
impl PageTableLevel for PDPT {
	const LEVEL: usize = 2;
}

impl PageTableLevelWithSubtables for PDPT {
	type SubtableLevel = PDT;
}

/// A Page Directory Table (PDT), with numeric level 1 and PGT subtables.
enum PDT {}
impl PageTableLevel for PDT {
	const LEVEL: usize = 1;
}

impl PageTableLevelWithSubtables for PDT {
	type SubtableLevel = PGT;
}

/// A Page Table (PGT), with numeric level 0 and no subtables.
enum PGT {}
impl PageTableLevel for PGT {
	const LEVEL: usize = 0;
}

/// Representation of any page table (PML4, PDPT, PDT, PGT) in memory.
/// Parameter L supplies information for Rust's typing system to distinguish between the different tables.
struct PageTable<L> {
	/// Each page table has 512 entries (can be calculated using PAGE_MAP_BITS).
	entries: [PageTableEntry; 1 << PAGE_MAP_BITS],

	/// Required by Rust to support the L parameter.
	level: PhantomData<L>,
}

/// A trait defining methods every page table has to implement.
/// This additional trait is necessary to make use of Rust's specialization feature and provide a default
/// implementation of some methods.
trait PageTableMethods {
	fn get_page_table_entry<S: PageSize>(&self, page: Page<S>) -> Option<PageTableEntry>;
	fn map_page_in_this_table<S: PageSize>(&mut self, page: Page<S>, physical_address: usize, flags: PageTableEntryFlags) -> bool;
	fn map_page<S: PageSize>(&mut self, page: Page<S>, physical_address: usize, flags: PageTableEntryFlags) -> bool;
	fn unmap_page_in_this_table<S: PageSize>(&mut self, page: Page<S>);
	fn unmap_page<S: PageSize>(&mut self, page: Page<S>);
}

impl<L: PageTableLevel> PageTableMethods for PageTable<L> {
	/// Maps a single page in this table to the given physical address.
	/// Returns whether an existing entry was updated. You can use this return value to flush TLBs.
	///
	/// Must only be called if a page of this size is mapped at this page table level!
	fn map_page_in_this_table<S: PageSize>(&mut self, page: Page<S>, physical_address: usize, flags: PageTableEntryFlags) -> bool {
		assert!(L::LEVEL == S::MAP_LEVEL);
		let index = page.table_index::<L>();
		let flush = self.entries[index].is_present();

		self.entries[index].set(physical_address, PageTableEntryFlags::DIRTY | S::MAP_EXTRA_FLAG | flags);

		if flush {
			unsafe { page.flush_from_tlb() };
		}

		flush
	}

	/// Unmaps a single page in this table.
	///
	/// Must only be called if a page of this size is mapped at this page table level!
	fn unmap_page_in_this_table<S: PageSize>(&mut self, page: Page<S>) {
		assert!(L::LEVEL == S::MAP_LEVEL);
		let index = page.table_index::<L>();

		self.entries[index].zero();
		unsafe { page.flush_from_tlb() };
	}

	/// Returns the PageTableEntry for the given page if it is present, otherwise returns None.
	///
	/// This is the default implementation called only for PGT.
	/// It is overridden by a specialized implementation for all tables with sub tables (all except PGT).
	default fn get_page_table_entry<S: PageSize>(&self, page: Page<S>) -> Option<PageTableEntry> {
		assert!(L::LEVEL == S::MAP_LEVEL);
		let index = page.table_index::<L>();

		if self.entries[index].is_present() {
			Some(self.entries[index])
		} else {
			None
		}
	}

	/// Maps a single page to the given physical address.
	/// Returns whether an existing entry was updated. You can use this return value to flush TLBs.
	///
	/// This is the default implementation that just calls the map_page_in_this_table method.
	/// It is overridden by a specialized implementation for all tables with sub tables (all except PGT).
	default fn map_page<S: PageSize>(&mut self, page: Page<S>, physical_address: usize, flags: PageTableEntryFlags) -> bool {
		self.map_page_in_this_table::<S>(page, physical_address, flags)
	}

	/// Unmaps a single page.
	///
	/// This is the default implementation that just calls the unmap_page_in_this_table method.
	/// It is overridden by a specialized implementation for all tables with sub tables (all except PGT).
	default fn unmap_page<S: PageSize>(&mut self, page: Page<S>) {
		self.unmap_page_in_this_table::<S>(page);
	}
}

impl<L: PageTableLevelWithSubtables> PageTableMethods for PageTable<L> where L::SubtableLevel: PageTableLevel {
	/// Returns the PageTableEntry for the given page if it is present, otherwise returns None.
	///
	/// This is the implementation for all tables with subtables (PML4, PDPT, PDT).
	/// It overrides the default implementation above.
	fn get_page_table_entry<S: PageSize>(&self, page: Page<S>) -> Option<PageTableEntry> {
		assert!(L::LEVEL >= S::MAP_LEVEL);
		let index = page.table_index::<L>();

		if self.entries[index].is_present() {
			if L::LEVEL > S::MAP_LEVEL {
				let subtable = self.subtable::<S>(page);
				subtable.get_page_table_entry::<S>(page)
			} else {
				Some(self.entries[index])
			}
		} else {
			None
		}
	}

	/// Maps a single page to the given physical address.
	/// Returns whether an existing entry was updated. You can use this return value to flush TLBs.
	///
	/// This is the implementation for all tables with subtables (PML4, PDPT, PDT).
	/// It overrides the default implementation above.
	fn map_page<S: PageSize>(&mut self, page: Page<S>, physical_address: usize, flags: PageTableEntryFlags) -> bool {
		assert!(L::LEVEL >= S::MAP_LEVEL);

		if L::LEVEL > S::MAP_LEVEL {
			let index = page.table_index::<L>();

			// Does the table exist yet?
			if !self.entries[index].is_present() {
				// Allocate a single 4KiB page for the new entry and mark it as a valid, writable subtable.
				let physical_address = unsafe { get_pages(1) };
				self.entries[index].set(physical_address, PageTableEntryFlags::WRITABLE);

				// Mark all entries as unused in the newly created table.
				let subtable = self.subtable::<S>(page);
				for entry in subtable.entries.iter_mut() {
					entry.zero();
				}
			}

			let subtable = self.subtable::<S>(page);
			subtable.map_page::<S>(page, physical_address, flags)
		} else {
			// Calling the default implementation from a specialized one is not supported (yet),
			// so we have to resort to an extra function.
			self.map_page_in_this_table::<S>(page, physical_address, flags)
		}
	}

	/// Unmaps a single page.
	///
	/// This is the implementation for all tables with subtables (PML4, PDPT, PDT).
	/// It overrides the default implementation above.
	fn unmap_page<S: PageSize>(&mut self, page: Page<S>) {
		assert!(L::LEVEL >= S::MAP_LEVEL);

		if L::LEVEL > S::MAP_LEVEL {
			let index = page.table_index::<L>();
			assert!(self.entries[index].is_present());

			let subtable = self.subtable::<S>(page);
			subtable.unmap_page::<S>(page);
		} else {
			// Calling the default implementation from a specialized one is not supported (yet),
			// so we have to resort to an extra function.
			self.unmap_page_in_this_table::<S>(page);
		}
	}
}

impl<L: PageTableLevelWithSubtables> PageTable<L> where L::SubtableLevel: PageTableLevel {
	/// Returns the next subtable for the given page in the page table hierarchy.
	///
	/// Must only be called if a page of this size is mapped in a subtable!
	fn subtable<S: PageSize>(&self, page: Page<S>) -> &mut PageTable<L::SubtableLevel> {
		assert!(L::LEVEL > S::MAP_LEVEL);

		// Calculate the address of the subtable.
		let index = page.table_index::<L>();
		let table_address = self as *const PageTable<L> as usize;
		let subtable_address = (table_address << PAGE_MAP_BITS) | (index << PAGE_BITS);
		unsafe { &mut *(subtable_address as *mut PageTable<L::SubtableLevel>) }
	}

	/// Maps a continuous range of pages.
	///
	/// # Arguments
	///
	/// * `range` - The range of pages of size S
	/// * `physical_address` - First physical address to map these pages to
	/// * `flags` - Flags from PageTableEntryFlags to set for the page table entry (e.g. WRITABLE or EXECUTE_DISABLE).
	///             The PRESENT, ACCESSED, and DIRTY flags are already set automatically.
	/// * `do_ipi` - Whether to flush the TLB of the other CPUs as well if existing entries were updated.
	fn map_pages<S: PageSize>(&mut self, range: PageIter<S>, physical_address: usize, flags: PageTableEntryFlags, do_ipi: bool) {
		let mut current_physical_address = physical_address;
		let mut send_ipi = false;

		for page in range {
			send_ipi |= self.map_page::<S>(page, current_physical_address, flags);
			current_physical_address += S::SIZE;
		}

		if do_ipi && send_ipi {
			unsafe { ipi_tlb_flush() };
		}
	}

	/// Unmaps a continuous range of pages.
	fn unmap_pages<S: PageSize>(&mut self, range: PageIter<S>) {
		for page in range {
			self.unmap_page::<S>(page);
		}

		unsafe { ipi_tlb_flush() };
	}
}

bitflags! {
	/// Possible flags for the error code of a Page-Fault Exception.
	///
	/// See Intel Vol. 3A, Figure 4-12
	struct PageFaultError: u64 {
		/// Set if the page fault was caused by a protection violation. Otherwise, it was caused by a non-present page.
		const PROTECTION_VIOLATION = 1 << 1;

		/// Set if the page fault was caused by a write operation. Otherwise, it was caused by a read operation.
		const WRITE = 1 << 2;

		/// Set if the page fault was caused in User Mode (Ring 3). Otherwise, it was caused in supervisor mode.
		const USER_MODE = 1 << 3;

		/// Set if the page fault was caused by writing 1 to a reserved field.
		const RESERVED_FIELD = 1 << 4;

		/// Set if the page fault was caused by an instruction fetch.
		const INSTRUCTION_FETCH = 1 << 5;
	}
}

impl fmt::Display for PageFaultError {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let mode = if self.contains(PageFaultError::USER_MODE) { "user" } else { "supervisor" };
		let ty = if self.contains(PageFaultError::INSTRUCTION_FETCH) { "instruction" } else { "data" };
		let operation = if self.contains(PageFaultError::WRITE) { "write" } else if self.contains(PageFaultError::INSTRUCTION_FETCH) { "fetch" } else { "read" };
		let cause = if self.contains(PageFaultError::PROTECTION_VIOLATION) { "protection" } else { "not present" };
		let reserved = if self.contains(PageFaultError::RESERVED_FIELD) { "reserved bit" } else { "\x08" };

		write!(f, "{:#X} [ {} {} {} {} {} ]", self.bits, mode, ty, operation, cause, reserved)
	}
}



#[inline]
fn get_page_range(viraddr: usize, npages: usize) -> PageIter<BasePageSize> {
	let first_page = Page::<BasePageSize>::including_address(viraddr);
	let last_page = Page::<BasePageSize>::including_address(viraddr + (npages - 1) * BasePageSize::SIZE);
	Page::<BasePageSize>::range(first_page, last_page)
}

#[no_mangle]
pub extern "C" fn getpagesize() -> i32 {
	BasePageSize::SIZE as i32
}

#[no_mangle]
pub unsafe extern "C" fn page_fault_handler(s: *const state) {
	debug!("page_fault_handler({:#X})", s as usize);

	let virtual_address = control_regs::cr2();
	let task = current_task.per_core().as_ref().expect("No task in page_fault_handler");

	if !task.heap.is_null() && virtual_address >= (*task.heap).start && virtual_address < (*task.heap).end {
		let mut locked_root_table = ROOT_PAGETABLE.lock();
		let page = Page::<BasePageSize>::including_address(virtual_address);

		if locked_root_table.get_page_table_entry(page).is_none() {
			let physical_address = if runtime_osinit.is_null() { get_pages(1) } else { get_zeroed_page() };
			locked_root_table.map_page::<BasePageSize>(page, physical_address, PageTableEntryFlags::WRITABLE | PageTableEntryFlags::EXECUTE_DISABLE);
		}
	} else {
		let pferror = PageFaultError { bits: (*s).error };

		error!("Page Fault Exception ({}) on core {} at cs:ip = {:#X}:{:#X}, fs = {:#X}, gs = {:#X}, rflags = {:#X}, task = {}, virtual_address = {:#X}, error = {}",
			(*s).int_no, __core_id.per_core(), (*s).cs, (*s).rip, (*s).fs, (*s).gs, (*s).rflags, task.id, virtual_address, pferror);
		error!("rax = {:#X}, rbx = {:#X}, rcx = {:#X}, rdx = {:#X}, rbp = {:#X}, rsp = {:#X}, rdi = {:#X}, rsi = {:#X}, r8 = {:#X}, r9 = {:#X}, r10 = {:#X}, r11 = {:#X}, r12 = {:#X}, r13 = {:#X}, r14 = {:#X}, r15 = {:#X}",
			(*s).rax, (*s).rbx, (*s).rcx, (*s).rdx, (*s).rbp, (*s).rsp, (*s).rdi, (*s).rsi, (*s).r8, (*s).r9, (*s).r10, (*s).r11, (*s).r12, (*s).r13, (*s).r14, (*s).r15);

		if !task.heap.is_null() {
			error!("Heap {:#X} - {:#X}", (*task.heap).start, (*task.heap).end);
		}

		apic_eoi((*s).int_no as usize);
		panic!();
	}
}

#[no_mangle]
pub extern "C" fn __page_map(viraddr: usize, phyaddr: usize, npages: usize, bits: usize, do_ipi: u8) -> i32 {
	debug!("__page_map({:#X}, {:#X}, {}, {:#X}, {})", viraddr, phyaddr, npages, bits, do_ipi);

	let range = get_page_range(viraddr, npages);
	ROOT_PAGETABLE.lock().map_pages(range, phyaddr, PageTableEntryFlags::from_bits_truncate(bits), do_ipi > 0);
	0
}

#[no_mangle]
pub extern "C" fn page_unmap(viraddr: usize, npages: usize) -> i32 {
	debug!("page_unmap({:#X}, {})", viraddr, npages);

	if npages > 0 {
		let range = get_page_range(viraddr, npages);
		ROOT_PAGETABLE.lock().unmap_pages(range);
	}

	0
}

pub fn map_cmdline() {
	unsafe {
		if cmdsize > 0 {
			// Add a read-only, execute-disable page mapping to enable access to the provided command line.
			let first_page = Page::<BasePageSize>::including_address(cmdline as usize);
			let last_page = Page::<BasePageSize>::including_address(cmdline as usize + cmdsize - 1);
			let range = Page::<BasePageSize>::range(first_page, last_page);

			ROOT_PAGETABLE.lock().map_pages(range, cmdline as usize & !(BasePageSize::SIZE - 1), PageTableEntryFlags::EXECUTE_DISABLE, true);
		}
	}
}

#[no_mangle]
pub unsafe extern "C" fn page_init() -> i32 {
	if !runtime_osinit.is_null() {
		info!("Detected Go runtime! HermitCore will return zeroed pages.");
	}

	irq_install_handler(14, page_fault_handler);
	0
}

#[no_mangle]
pub unsafe extern "C" fn virt_to_phys(addr: usize) -> usize {
	debug!("virt_to_phys({:#X})", addr);

	// HACK: Currently, we use 2MiB pages only for the kernel.
	let kernel_end: usize = ((&kernel_start as *const u8 as usize + image_size) & !(LargePageSize::SIZE - 1)).saturating_add(LargePageSize::SIZE);

	if addr >= (&kernel_start as *const u8 as usize) && addr <= kernel_end {
		let page = Page::<LargePageSize>::including_address(addr);
		let address = ROOT_PAGETABLE.lock().get_page_table_entry(page).expect("Entry not present").address();
		let offset = addr & (LargePageSize::SIZE - 1);
		address | offset
	} else {
		let page = Page::<BasePageSize>::including_address(addr);
		let address = ROOT_PAGETABLE.lock().get_page_table_entry(page).expect("Entry not present").address();
		let offset = addr & (BasePageSize::SIZE - 1);
		address | offset
	}
}