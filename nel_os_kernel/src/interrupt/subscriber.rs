use alloc::vec::Vec;
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

static SUBSCRIBERS: Mutex<Vec<Subscriber>> = Mutex::new(Vec::new());

pub fn subscribe(
    callback: SubscriberCallback,
    context: *mut core::ffi::c_void,
) -> Result<(), &'static str> {
    let mut subscribers = SUBSCRIBERS.lock();
    subscribers
        .try_reserve(1)
        .map_err(|_| "Unable to allocate an interrupt subscriber")?;
    subscribers.push(Subscriber { callback, context });
    Ok(())
}

pub fn unsubscribe(callback: SubscriberCallback) -> Result<(), &'static str> {
    let mut subscribers = SUBSCRIBERS.lock();

    if let Some(index) = subscribers
        .iter()
        .position(|subscriber| core::ptr::fn_addr_eq(subscriber.callback, callback))
    {
        subscribers.swap_remove(index);
        return Ok(());
    }

    Err("Subscriber not found")
}

pub fn unsubscribe_context(
    callback: SubscriberCallback,
    context: *mut core::ffi::c_void,
) -> Result<(), &'static str> {
    let mut subscribers = SUBSCRIBERS.lock();
    if let Some(index) = subscribers.iter().position(|subscriber| {
        core::ptr::fn_addr_eq(subscriber.callback, callback) && subscriber.context == context
    }) {
        subscribers.swap_remove(index);
        Ok(())
    } else {
        Err("Subscriber not found")
    }
}

pub fn dispatch_to_subscribers(context: &InterruptContext) {
    let subscribers = SUBSCRIBERS.lock();

    for subscriber in subscribers.iter() {
        (subscriber.callback)(subscriber.context, context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn callback(_context: *mut core::ffi::c_void, _interrupt: &InterruptContext) {}

    #[test]
    fn registry_grows_beyond_the_previous_fixed_limit() {
        for _ in 0..32 {
            subscribe(callback, core::ptr::null_mut()).unwrap();
        }
        for _ in 0..32 {
            unsubscribe(callback).unwrap();
        }
    }
}
