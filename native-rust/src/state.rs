#[derive(Clone, Copy, Debug)]
pub enum Event {
    Start,
    Arm,
    Pause,
    Stop,
    Sleep,
    Wake,
    Lock,
    Unlock,
    Inactive,
    Active,
}
#[derive(Default, Debug)]
pub struct Gate {
    pub armed: bool,
    pub manual: bool,
    pub sleeping: bool,
    pub locked: bool,
    pub inactive: bool,
}
impl Gate {
    pub fn apply(&mut self, e: Event) {
        match e {
            Event::Start => {
                self.armed = true;
                self.manual = true
            }
            Event::Arm => {
                self.armed = true;
                self.manual = false
            }
            Event::Pause | Event::Stop => {
                self.armed = false;
                self.manual = false
            }
            Event::Sleep => self.sleeping = true,
            Event::Wake => self.sleeping = false,
            Event::Lock => self.locked = true,
            Event::Unlock => self.locked = false,
            Event::Inactive => self.inactive = true,
            Event::Active => self.inactive = false,
        }
    }
    pub fn blocked(&self) -> bool {
        self.sleeping || self.locked || self.inactive
    }
    pub fn eligible(&self, hours: bool) -> bool {
        self.armed && !self.blocked() && (self.manual || hours)
    }
}
pub fn verify() {
    let mut g = Gate::default();
    assert!(!g.eligible(true));
    g.apply(Event::Start);
    assert!(g.eligible(false));
    g.apply(Event::Lock);
    g.apply(Event::Sleep);
    g.apply(Event::Wake);
    g.apply(Event::Inactive);
    g.apply(Event::Active);
    assert!(!g.eligible(true));
    g.apply(Event::Unlock);
    assert!(g.eligible(false));
    g.apply(Event::Stop);
    g.apply(Event::Wake);
    g.apply(Event::Unlock);
    assert!(!g.eligible(true));
    g.apply(Event::Arm);
    assert!(!g.eligible(false));
    assert!(g.eligible(true));
    g.apply(Event::Pause);
    g.apply(Event::Lock);
    g.apply(Event::Unlock);
    assert!(!g.eligible(true));
}
#[cfg(test)]
mod tests {
    #[test]
    fn manual_stop_and_lock_ordering() {
        super::verify()
    }
}
