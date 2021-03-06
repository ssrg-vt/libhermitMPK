#![feature(asm)]

extern crate http;
extern crate rayon;

mod tests;

use std::io::Write;
use std::sync::{Arc,Mutex};
use std::sync::atomic::{AtomicU32, Ordering};
use tests::*;

fn test_result<T>(result: Result<(), T>) -> &'static str {
	match result {
		Ok(_) => "ok",
		Err(_) => "failed!",
	}
}

fn test_syscall_cost() {
	use std::time::Instant;
	use std::process::id;
	let now = Instant::now();
	for _ in 0..100000000 {
		let _ = id();
	}
	let elapsed = now.elapsed().as_secs_f64();
	println!("getpid {} s", elapsed);
}

fn test_syscall_cost2() {
	extern "C" {
		fn sys_getpid() -> u32;
		//fn sys_getprio(id: *const u32) -> i32;
	}

	use std::time::Instant;
	let now = Instant::now();
	for _ in 0..100000000 {
		unsafe {
			let _ = sys_getpid();
		}
	}
	let elapsed = now.elapsed().as_secs_f64();
	println!("sys_getpid {} s", elapsed);
}

fn vulnerable_function(string: String, address: *mut String) {
	unsafe {
		println!("bafore writing");
		*address = string;
		println!("after writing");
	}
}

fn security_evaluation_user_isolation() {
	let s = "hello".to_string();
	vulnerable_function(s, 0x400000usize as *mut _);
}

//static COUNTER: AtomicU32 = AtomicU32::new(8);
//static results: Mutex<Vec<f64>> = Mutex::new(Vec::<f64>::new());
/*
fn test_threading() -> Result<(), ()> {
	use std::thread;
	use std::time::Instant;
	use std::process::id;

	extern "C" {
		fn sys_getpid() -> u32;
		fn sys_getprio(id: *const u32) -> i32;
	}

	// Make a vector to hold the children which are spawned.
	let mut children = vec![];
        let results = Arc::new(Mutex::new(vec![]));
	//let now = Instant::now();

	for i in 0..8 {
                let results = results.clone();
                let now = Instant::now();

		// Spin up another thread
		children.push(thread::spawn(move || {
			unsafe {
				for _ in 0..(10000000/8) {
					let _ = sys_getpid();
				}
			}
                        let elapsed = now.elapsed().as_secs_f64();
                        results.lock().unwrap().push(elapsed);

                        //println!("id:{} COUNTER: {} {}", id, COUNTER.load(Ordering::SeqCst), now.elapsed().as_secs_f64());
                        COUNTER.fetch_sub(1, Ordering::SeqCst);
		}));
	}

	println!("before join");
        while COUNTER.load(Ordering::Relaxed) > 0 {
            unsafe { asm!("pause" :::: "volatile"); }
        }
        while let Some(i) = results.lock().unwrap().pop() {
            println!("{}", i);
        }

	/*for child in children {
		// Wait for the thread to finish. Returns a result.
		let _ = child.join();
	}*/

	Ok(())
}
*/

fn main() {
        println!("Test {} ... {}", stringify!(hello), test_result(hello()));

/*	
        test_syscall_cost();
	test_syscall_cost2();
        test_threading();
	security_evaluation_user_isolation();
        
        println!(
		"Test {} ... {}",
		stringify!(test_pkru_context_switch),
		test_result(test_pkru_context_switch())
	);

	println!(
		"Test {} ... {}",
		stringify!(print_argv),
		test_result(print_argv())
	);
	println!(
		"Test {} ... {}",
		stringify!(print_env),
		test_result(print_env())
	);

	println!(
		"Test {} ... {}",
		stringify!(read_file),
		test_result(read_file())
	);
	println!(
		"Test {} ... {}",
		stringify!(create_file),
		test_result(create_file())
	);

        println!("before alloc");
        unsafe {
        let layout: std::alloc::Layout = std::alloc::Layout::from_size_align(8, 8).unwrap();
        let a = std::alloc::alloc(layout);
        }
        println!("after alloc");
        println!(
		"Test {} ... {}",
		stringify!(threading),
                test_result(threading())
	);
	
        println!(
		"Test {} ... {}",
		stringify!(pi_sequential),
		test_result(pi_sequential(1000000))
	);

	println!(
		"Test {} ... {}",
		stringify!(pi_parallel),
		test_result(pi_parallel(2, 5000000))
	);
	println!(
		"Test {} ... {}",
		stringify!(laplace),
		test_result(laplace(128, 128))
	);

	println!(
		"Test {} ... {}",
		stringify!(test_matmul_strassen),
		test_result(test_matmul_strassen())
	);
	println!(
		"Test {} ... {}",
		stringify!(thread_creation),
		test_result(thread_creation())
	);

	println!(
		"Test {} ... {}",
		stringify!(bench_sched_one_thread),
		test_result(bench_sched_one_thread())
	);

        println!(
		"Test {} ... {}",
		stringify!(bench_sched_two_threads),
		test_result(bench_sched_two_threads())
	);
	println!(
		"Test {} ... {}",
		stringify!(test_http_request),
		test_result(test_http_request())
	);
*/
}
