#[derive(Debug, Default)]
pub struct Session {
    busy: bool,
    owner: Option<u32>,
}

impl Session {
    pub fn press(&mut self, shortcut: u32) -> bool {
        if self.busy {
            return false;
        }
        self.busy = true;
        self.owner = Some(shortcut);
        true
    }

    pub fn release(&mut self, shortcut: u32) -> bool {
        if self.owner != Some(shortcut) {
            return false;
        }
        self.owner = None;
        true
    }

    pub fn finish(&mut self) {
        self.owner = None;
        self.busy = false;
    }
}

#[cfg(test)]
mod tests {
    use super::Session;

    #[test]
    fn presses_and_releases_during_transcription_do_not_unlock_session() {
        let mut s = Session::default();
        assert!(s.press(1));
        assert!(s.release(1));
        assert!(!s.press(1));
        assert!(!s.release(1));
        assert!(!s.press(2));
        assert!(!s.release(2));
        assert!(!s.press(1));
        s.finish();
        assert!(s.press(1));
        assert!(s.release(1));
    }

    #[test]
    fn other_shortcut_and_repeat_cannot_take_over_recording() {
        let mut s = Session::default();
        assert!(s.press(1));
        assert!(!s.press(1));
        assert!(!s.press(2));
        assert!(!s.release(2));
        assert!(s.release(1));
        assert!(!s.release(1));
        s.finish();
        assert!(s.press(2));
    }

    #[test]
    fn failed_start_can_be_retried() {
        let mut s = Session::default();
        assert!(s.press(1));
        s.finish();
        assert!(!s.release(1));
        assert!(s.press(1));
    }
}
