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

static SUBSCRIBERS: Mutex<[Option<Subscriber>; MAX_SUBSCRIBERS]> =
    Mutex::new([None; MAX_SUBSCRIBERS]);

pub fn subscribe(
    callback: SubscriberCallback,
    context: *mut core::ffi::c_void,
) -> Result<(), &'static str> {
    let mut subscribers = SUBSCRIBERS.lock();

    for slot in subscribers.iter_mut() {
        if slot.is_none() {
            *slot = Some(Subscriber { callback, context });
            return Ok(());
        }
    }

    Err("No available subscriber slots")
}

pub fn unsubscribe(callback: SubscriberCallback) -> Result<(), &'static str> {
    let mut subscribers = SUBSCRIBERS.lock();

    for slot in subscribers.iter_mut() {
        if let Some(subscriber) = slot {
            if subscriber.callback == callback {
                *slot = None;
                return Ok(());
            }
        }
    }

    Err("Subscriber not found")
}

pub fn dispatch_to_subscribers(context: &InterruptContext) {
    let subscribers = SUBSCRIBERS.lock();

    for subscriber in subscribers.iter() {
        if let Some(subscriber) = subscriber {
            (subscriber.callback)(subscriber.context, context);
        }
    }
}
