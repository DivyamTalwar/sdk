#[test]
fn delivery_registration_cannot_lose_a_wakeup() {
    registration(false);
}
#[test]
#[should_panic(expected = "a parked receiver must be woken")]
fn delivery_negative_control_omits_notification() {
    registration(true);
}
fn registration(broken: bool) {
    loom::model(move || {
        let signal = super::Arc::new(Signal::new());
        let (count, wake) = super::wake_count();
        let reader = super::Arc::clone(&signal);
        let reader =
            loom::thread::spawn(move || match reader.poll(&mut Context::from_waker(&wake)) {
                Poll::Ready(value) => Some(value.unwrap()),
                Poll::Pending => None,
            });
        if broken {
            let mut state = signal.state.lock().unwrap();
            state.0.push_back(7_u8);
            state.1.take();
        } else {
            signal.send(7_u8).unwrap();
        }
        let received = reader.join().unwrap();
        assert!(
            received.is_some() || count.0.load(super::Ordering::SeqCst) > 0,
            "a parked receiver must be woken"
        );
        assert_eq!(
            received.unwrap_or_else(|| loom::future::block_on(signal.next()).unwrap()),
            7
        );
    });
}
