use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

#[derive(Debug)]
pub struct InterruptContext {
    pub vector: u8,
    pub instruction_pointer: u64,
    pub code_segment: u64,
    pub cpu_flags: u64,
    pub stack_pointer: u64,
    pub stack_segment: u64,
}

pub type SubscriberCallback = fn(*mut core::ffi::c_void, &InterruptContext);

#[derive(Debug, Clone, Copy)]
pub struct Subscriber {
    pub callback: SubscriberCallback,
    pub context: *mut core::ffi::c_void,
}

unsafe impl Send for Subscriber {}
unsafe impl Sync for Subscriber {}

const MAX_SUBSCRIBERS: usize = 10;

struct SubscriberArray {
    data: UnsafeCell<[Option<Subscriber>; MAX_SUBSCRIBERS]>,
    lock: Mutex<()>,
}

unsafe impl Sync for SubscriberArray {}

static SUBSCRIBERS: SubscriberArray = SubscriberArray {
    data: UnsafeCell::new([None; MAX_SUBSCRIBERS]),
    lock: Mutex::new(()),
};
static SUBSCRIBER_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn subscribe(
    callback: SubscriberCallback,
    context: *mut core::ffi::c_void,
) -> Result<(), &'static str> {
    let _lock = SUBSCRIBERS.lock.lock();

    unsafe {
        let subscribers = &mut *SUBSCRIBERS.data.get();

        for slot in subscribers.iter_mut() {
            if slot.is_none() {
                *slot = Some(Subscriber { callback, context });
                SUBSCRIBER_COUNT.fetch_add(1, Ordering::Release);
                return Ok(());
            }
        }
    }

    Err("No available subscriber slots")
}

pub fn unsubscribe(callback: SubscriberCallback) -> Result<(), &'static str> {
    let _lock = SUBSCRIBERS.lock.lock();

    unsafe {
        let subscribers = &mut *SUBSCRIBERS.data.get();

        for slot in subscribers.iter_mut() {
            if let Some(subscriber) = slot {
                if core::ptr::fn_addr_eq(subscriber.callback, callback) {
                    *slot = None;
                    SUBSCRIBER_COUNT.fetch_sub(1, Ordering::Release);
                    return Ok(());
                }
            }
        }
    }

    Err("Subscriber not found")
}

#[inline]
pub fn dispatch_to_subscribers(context: &InterruptContext) {
    // Early return if no subscribers
    let count = SUBSCRIBER_COUNT.load(Ordering::Acquire);
    if count == 0 {
        return;
    }

    unsafe {
        let subscribers = &*SUBSCRIBERS.data.get();

        for subscriber in subscribers.iter().flatten() {
            (subscriber.callback)(subscriber.context, context);
        }
    }
}
