//! Typed native scanner, parser and conversion work authority.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stop {
    Cancelled,
    Work,
    Tokens,
    Positions,
    Comments,
    SourceLength,
    Allocation,
    InvalidPosition,
    InvalidMode,
    Depth,
    Nodes,
    Invariant,
    UnsupportedRegexValidation,
    UnsupportedTimePrecision,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub source_bytes: usize,
    pub transitions: usize,
    pub tokens: usize,
    pub positions: usize,
    pub comments: usize,
    pub parser_depth: usize,
    pub ast_nodes: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Progress {
    pub transitions: usize,
    pub tokens: usize,
    pub comments: usize,
    pub parser_steps: usize,
    pub conversion_steps: usize,
    pub parser_depth: usize,
    pub ast_nodes: usize,
    pub byte_offset: usize,
}

pub struct Control<'a> {
    pub limits: Limits,
    progress: Progress,
    stopped: Option<Stop>,
    cancel: &'a mut dyn FnMut(Progress) -> bool,
}

impl<'a> Control<'a> {
    pub fn new(limits: Limits, cancel: &'a mut dyn FnMut(Progress) -> bool) -> Self {
        Self {
            limits,
            progress: Progress::default(),
            stopped: None,
            cancel,
        }
    }

    pub fn progress(&self) -> Progress {
        self.progress
    }

    pub fn check(&mut self) -> Result<(), Stop> {
        if let Some(stop) = self.stopped {
            return Err(stop);
        }
        if (self.cancel)(self.progress) {
            return self.stop(Stop::Cancelled);
        }
        Ok(())
    }

    pub fn stop<T>(&mut self, stop: Stop) -> Result<T, Stop> {
        Err(self.stop_reason(stop))
    }

    pub fn stop_reason(&mut self, stop: Stop) -> Stop { *self.stopped.get_or_insert(stop) }

    pub fn conversion(&mut self) -> Result<(), Stop> {
        self.transition(self.progress.byte_offset as i32)?;
        self.progress.conversion_steps += 1;
        Ok(())
    }

    pub fn transition(&mut self, offset: i32) -> Result<(), Stop> {
        self.check()?;
        let offset = match usize::try_from(offset) {
            Ok(value) => value,
            Err(_) => return self.stop(Stop::InvalidPosition),
        };
        if self.progress.transitions >= self.limits.transitions {
            return self.stop(Stop::Work);
        }
        self.progress.transitions += 1;
        self.progress.byte_offset = offset;
        Ok(())
    }

    /// Charges each actual generated key access, including repeated comparisons and revisits.
    pub fn key(&mut self, source: &[u8], offset: i32) -> Result<u8, Stop> {
        self.transition(offset)?;
        match usize::try_from(offset)
            .ok()
            .and_then(|offset| source.get(offset))
            .copied()
        {
            Some(byte) => Ok(byte),
            None => self.stop(Stop::InvalidPosition),
        }
    }

    pub fn parser_step(&mut self) -> Result<(), Stop> {
        self.transition(self.progress.byte_offset as i32)?;
        self.progress.parser_steps += 1;
        Ok(())
    }

    pub fn enter(&mut self) -> Result<(), Stop> {
        self.parser_step()?;
        if self.progress.parser_depth >= self.limits.parser_depth { return self.stop(Stop::Depth); }
        self.progress.parser_depth += 1;
        Ok(())
    }

    pub fn leave(&mut self) -> Result<(), Stop> {
        if self.progress.parser_depth == 0 { return self.stop(Stop::Invariant); }
        self.progress.parser_depth -= 1;
        Ok(())
    }

    pub fn node(&mut self) -> Result<(), Stop> {
        self.check()?;
        if self.progress.ast_nodes >= self.limits.ast_nodes { return self.stop(Stop::Nodes); }
        self.progress.ast_nodes += 1;
        Ok(())
    }

    pub fn token(&mut self) -> Result<(), Stop> {
        self.check()?;
        if self.progress.tokens >= self.limits.tokens {
            return self.stop(Stop::Tokens);
        }
        self.progress.tokens += 1;
        Ok(())
    }

    pub fn comment(&mut self) -> Result<(), Stop> {
        self.check()?;
        if self.progress.comments >= self.limits.comments {
            return self.stop(Stop::Comments);
        }
        self.progress.comments += 1;
        Ok(())
    }
}

pub fn checked_source_len(length: usize, maximum: usize) -> Result<i32, Stop> {
    // Native line/column and post-transition pointers also require one spare i32 value.
    if length > maximum || length >= i32::MAX as usize {
        return Err(Stop::SourceLength);
    }
    i32::try_from(length).map_err(|_| Stop::SourceLength)
}
