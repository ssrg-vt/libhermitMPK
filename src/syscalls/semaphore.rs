// Copyright (c) 2018 Colin Finck, RWTH Aachen University
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use alloc::boxed::Box;
use arch;
use errno::*;
use synch::semaphore::Semaphore;
use mm;

#[no_mangle]
fn __sys_sem_init(sem: *mut *mut Semaphore, value: u32) -> i32 {
	//println!("sys_sem_init, sem: {:#X}", sem as usize);
	if sem.is_null() {
		return -EINVAL;
	}

	// Create a new boxed semaphore and return a pointer to the raw memory.
	let boxed_semaphore = Box::new(Semaphore::new(value as isize));
	let temp = Box::into_raw(boxed_semaphore);
	unsafe {
		isolation_start!();
		*sem = temp;
		isolation_end!();
	}
	0
}

#[no_mangle]
pub extern "C" fn sys_sem_init(sem: *mut *mut Semaphore, value: u32) -> i32 {
	let ret = kernel_function!(__sys_sem_init(sem, value));
	return ret;
}

#[no_mangle]
fn __sys_sem_destroy(sem: *mut Semaphore) -> i32 {
	if sem.is_null() {
		return -EINVAL;
	}

	// Consume the pointer to the raw memory into a Box again
	// and drop the Box to free the associated memory.
	/*unsafe {
		isolate_function_strong!(Box::from_raw(sem));
	}*/
	0
}

#[no_mangle]
pub extern "C" fn sys_sem_destroy(sem: *mut Semaphore) -> i32 {
	let ret = kernel_function!(__sys_sem_destroy(sem: *mut Semaphore));
	return ret;
}

#[no_mangle]
fn __sys_sem_post(sem: *const Semaphore) -> i32 {
	if sem.is_null() {
		return -EINVAL;
	}

	// Get a reference to the given semaphore and release it.
	let semaphore = unsafe {
								isolation_start!();
								let temp = &*sem;
								isolation_end!();
								temp
							};
	semaphore.release();
	0
}

#[no_mangle]
pub extern "C" fn sys_sem_post(sem: *const Semaphore) -> i32 {
	let ret = kernel_function!(__sys_sem_post(sem: *const Semaphore));
	return ret;
}

#[no_mangle]
fn __sys_sem_trywait(sem: *const Semaphore) -> i32 {
	if sem.is_null() {
		return -EINVAL;
	}

	// Get a reference to the given semaphore and acquire it in a non-blocking fashion.
	let semaphore = unsafe {
								isolation_start!();
								let temp = &*sem;
								isolation_end!();
								temp
							};
	if semaphore.try_acquire() {
		0
	} else {
		-ECANCELED
	}
}

#[no_mangle]
pub extern "C" fn sys_sem_trywait(sem: *const Semaphore) -> i32 {
	let ret = kernel_function!(__sys_sem_trywait(sem));
	return ret;
}

#[no_mangle]
fn __sys_sem_timedwait(sem: *const Semaphore, ms: u32) -> i32 {
	//println!("sys_sem_timedwait, sem: {:#X}", sem as usize);
	if sem.is_null() {
		return -EINVAL;
	}

	// Calculate the absolute wakeup time in processor timer ticks out of the relative timeout in milliseconds.
	let wakeup_time = if ms > 0 {
		Some(arch::processor::get_timer_ticks() + u64::from(ms) * 1000)
	} else {
		None
	};

	// Get a reference to the given semaphore and wait until we have acquired it or the wakeup time has elapsed.
	let semaphore = unsafe {
								isolation_start!();
								let temp = &*sem;
								isolation_end!();
								temp
							};
	if semaphore.acquire(wakeup_time) {
		0
	} else {
		-ETIME
	}
}

#[no_mangle]
pub extern "C" fn sys_sem_timedwait(sem: *const Semaphore, ms: u32) -> i32 {
	return kernel_function!(__sys_sem_timedwait(sem, ms));
}

#[no_mangle]
pub extern "C" fn sys_sem_cancelablewait(sem: *const Semaphore, ms: u32) -> i32 {
	sys_sem_timedwait(sem, ms)
}
