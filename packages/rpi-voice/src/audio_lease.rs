//! Exclusive access to the microphone and speakers across this image.
use super::RuntimeContext;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Direction {
    Capture,
    Playback,
}

static DEVICE: Mutex<Option<(usize, Direction)>> = Mutex::new(None);
pub(super) const BUSY: &str = "Voice audio is busy in another session";

pub(super) struct Lease;

impl Lease {
    pub(super) fn acquire(
        voice: &Arc<RuntimeContext>,
        direction: Direction,
        stop: &AtomicBool,
    ) -> Result<Self, String> {
        let id = Arc::as_ptr(voice) as usize;
        loop {
            if voice.is_stopping() || stop.load(Ordering::Acquire) {
                return Err(super::player::CANCELLED.into());
            }
            {
                let mut device = DEVICE.lock().unwrap();
                match *device {
                    None => {
                        *device = Some((id, direction));
                        return Ok(Self);
                    }
                    // Same-owner barge-in has already cancelled playback. Its
                    // worker must close the stream before the microphone opens.
                    Some((owner, Direction::Playback))
                        if owner == id && direction == Direction::Capture => {}
                    Some(_) => return Err(BUSY.into()),
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        *DEVICE.lock().unwrap() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_joins_the_audio_owner_before_another_registration_can_acquire() {
        let first = crate::registration::test_runtime();
        let second = crate::registration::test_runtime();
        let (ready, acquired) = std::sync::mpsc::channel();
        first
            .spawn("voice-audio-lease-test", move |voice| {
                let _lease = Lease::acquire(&voice, Direction::Playback, &voice.manual_stop)
                    .expect("first registration can acquire audio");
                ready.send(()).unwrap();
                while !voice.is_stopping() {
                    std::thread::park_timeout(Duration::from_millis(5));
                }
            })
            .unwrap();
        acquired.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            Lease::acquire(&second, Direction::Capture, &second.manual_stop),
            Err(error) if error == BUSY
        ));
        first.finish();
        assert!(first.workers.lock().unwrap().is_empty());
        assert!(!second.is_stopping());
        let _lease = Lease::acquire(&second, Direction::Capture, &second.manual_stop)
            .expect("audio released when first registration finished");
    }
}
