use sigmaos_component::{Config, Service};

#[test]
fn config_processes() {
    assert!(Config::default().process().is_ok());
}

#[test]
fn service_lifecycle() {
    let mut s = Service::new(Config::default());
    assert!(!s.is_running());
    s.start().unwrap();
    assert!(s.is_running());
    s.stop().unwrap();
    assert!(!s.is_running());
}

#[test]
fn double_start_fails() {
    let mut s = Service::new(Config::default());
    s.start().unwrap();
    assert!(s.start().is_err());
}

#[test]
fn stop_when_stopped_fails() {
    let mut s = Service::new(Config::default());
    assert!(s.stop().is_err());
}
