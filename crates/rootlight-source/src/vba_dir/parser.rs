//! Ordered MS-OVBA record reader over one bounded decompressed byte slice.
//! Several reserved size words are not lengths on read; the format-specific
//! layouts prevent record-marker bytes inside text from changing parser control.

use super::{
    Cancellation, DirectoryError, DirectoryLimits, ModuleKind, ModuleRecord, ProjectDirectory,
    ProjectVersionRecord, ReferenceName, ReferencePayload, ReferenceRecord, TextField,
};

pub(super) fn parse<'a>(
    bytes: &'a [u8],
    limits: DirectoryLimits,
    cancellation: &Cancellation,
) -> Result<ProjectDirectory<'a>, DirectoryError> {
    cancellation.check()?;
    if u64::try_from(limits.input_bytes).map_err(|_| DirectoryError::Limits)?
        > crate::HARD_MAX_SNAPSHOT_BYTES
        || limits.modules > usize::from(u16::MAX)
        || limits.references > usize::from(u16::MAX)
    {
        return Err(DirectoryError::Limits);
    }
    bound(bytes.len(), limits.input_bytes, "input bytes")?;
    let mut reader = Reader {
        bytes,
        position: 0,
        cancellation,
    };
    let (code_page, name, version) = reader.information()?;
    let information_range = 0..reader.position;
    let mut references = Vec::new();
    while reader.peek()? != 0x000f {
        bound(references.len() + 1, limits.references, "references")?;
        references.push(reader.reference()?);
    }
    reader.header(0x000f, 2)?;
    let count = usize::from(reader.u16()?);
    bound(count, limits.modules, "modules")?;
    reader.header(0x0013, 2)?;
    reader.take(2)?;
    let mut modules = Vec::new();
    for _ in 0..count {
        modules.push(reader.module()?);
    }
    reader.marker(0x0010)?;
    if reader.position != bytes.len() {
        return Err(reader.invalid());
    }
    cancellation.check()?;
    Ok(ProjectDirectory {
        bytes,
        information_range,
        code_page,
        name,
        version,
        references,
        modules,
    })
}

fn bound(value: usize, maximum: usize, resource: &'static str) -> Result<(), DirectoryError> {
    if value > maximum {
        Err(DirectoryError::Limit { resource, maximum })
    } else {
        Ok(())
    }
}

struct Reader<'a, 'c> {
    bytes: &'a [u8],
    position: usize,
    cancellation: &'c Cancellation,
}

impl<'a> Reader<'a, '_> {
    fn invalid(&self) -> DirectoryError {
        DirectoryError::Invalid {
            offset: self.position,
        }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], DirectoryError> {
        self.cancellation.check()?;
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| self.invalid())?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or(DirectoryError::Truncated {
                offset: self.position,
            })?;
        self.position = end;
        Ok(bytes)
    }

    fn u16(&mut self) -> Result<u16, DirectoryError> {
        let bytes = self.take(2)?.try_into().map_err(|_| self.invalid())?;
        Ok(u16::from_le_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, DirectoryError> {
        let bytes = self.take(4)?.try_into().map_err(|_| self.invalid())?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn peek(&mut self) -> Result<u16, DirectoryError> {
        let start = self.position;
        let id = self.u16()?;
        self.position = start;
        Ok(id)
    }

    fn id(&mut self, expected: u16) -> Result<(), DirectoryError> {
        let offset = self.position;
        let id = self.u16()?;
        if id == expected {
            Ok(())
        } else {
            Err(DirectoryError::Record { offset, id })
        }
    }

    fn value(&mut self, expected: u32) -> Result<(), DirectoryError> {
        let offset = self.position;
        if self.u32()? == expected {
            Ok(())
        } else {
            Err(DirectoryError::Invalid { offset })
        }
    }

    fn header(&mut self, id: u16, size: u32) -> Result<(), DirectoryError> {
        self.id(id)?;
        self.value(size)
    }

    fn marker(&mut self, id: u16) -> Result<(), DirectoryError> {
        self.id(id)?;
        self.take(4)?;
        Ok(())
    }

    fn optional_marker(&mut self, id: u16) -> Result<bool, DirectoryError> {
        let present = self.peek()? == id;
        if present {
            self.marker(id)?;
        }
        Ok(present)
    }

    fn text(&mut self, maximum: usize) -> Result<TextField<'a>, DirectoryError> {
        let size = usize::try_from(self.u32()?).map_err(|_| self.invalid())?;
        if size > maximum {
            return Err(self.invalid());
        }
        let start = self.position;
        let bytes = self.take(size)?;
        for chunk in bytes.chunks(4096) {
            self.cancellation.check()?;
            if chunk.contains(&0) {
                return Err(DirectoryError::Invalid { offset: start });
            }
        }
        Ok(TextField {
            bytes,
            range: start..self.position,
        })
    }

    fn unicode(&mut self) -> Result<TextField<'a>, DirectoryError> {
        let size = usize::try_from(self.u32()?).map_err(|_| self.invalid())?;
        if size % 2 != 0 {
            return Err(self.invalid());
        }
        let start = self.position;
        let bytes = self.take(size)?;
        let units = bytes.chunks_exact(2).map(|pair| {
            let [low, high] = pair else {
                unreachable!("chunks_exact yields two bytes")
            };
            u16::from_le_bytes([*low, *high])
        });
        for scalar in char::decode_utf16(units) {
            self.cancellation.check()?;
            if !matches!(scalar, Ok(character) if character != '\0') {
                return Err(DirectoryError::Invalid { offset: start });
            }
        }
        Ok(TextField {
            bytes,
            range: start..self.position,
        })
    }

    fn pair(
        &mut self,
        id: u16,
        maximum: usize,
    ) -> Result<(TextField<'a>, TextField<'a>), DirectoryError> {
        self.id(id)?;
        let mbcs = self.text(maximum)?;
        // MS-OVBA reader rules explicitly ignore this reserved word, even though
        // canonical writers use it as a Unicode suffix identifier.
        self.take(2)?;
        Ok((mbcs, self.unicode()?))
    }

    fn information(
        &mut self,
    ) -> Result<(u16, TextField<'a>, ProjectVersionRecord), DirectoryError> {
        self.header(1, 4)?;
        if self.u32()? > 3 {
            return Err(self.invalid());
        }
        if self.peek()? == 0x004a {
            self.header(0x004a, 4)?;
            self.take(4)?;
        }
        for id in [2, 0x0014] {
            self.header(id, 4)?;
            self.value(0x0409)?;
        }
        self.header(3, 2)?;
        let code_page = self.u16()?;
        self.id(4)?;
        let name = self.text(128)?;
        if name.bytes.is_empty() {
            return Err(self.invalid());
        }
        self.pair(5, 2000)?;
        self.id(6)?;
        let help = self.text(260)?;
        self.take(2)?;
        if self.text(260)?.bytes != help.bytes {
            return Err(self.invalid());
        }
        self.header(7, 4)?;
        self.take(4)?;
        self.header(8, 4)?;
        self.value(0)?;
        // PROJECTVERSION's reserved word is NOT a generic TLV length. The
        // following major/minor pair occupies six bytes regardless of its value.
        self.marker(9)?;
        let version_start = self.position;
        let major = self.u32()?;
        let minor = self.u16()?;
        let version = ProjectVersionRecord {
            major,
            minor,
            range: version_start..self.position,
        };
        if self.peek()? == 0x000c {
            self.pair(0x000c, 1015)?;
        }
        Ok((code_page, name, version))
    }

    fn reference(&mut self) -> Result<ReferenceRecord, DirectoryError> {
        let start = self.position;
        let name = self.reference_name()?;
        let offset = self.position;
        let id = self.peek()?;
        let payload = match id {
            0x002f => self.control(None)?,
            0x0033 => {
                self.id(0x0033)?;
                let original = self.text(self.bytes.len())?.range;
                self.control(Some(original))?
            }
            0x000d => {
                self.marker(0x000d)?;
                let libid = self.text(self.bytes.len())?.range;
                self.take(6)?;
                ReferencePayload::Registered { libid }
            }
            0x000e => {
                self.marker(0x000e)?;
                let absolute_libid = self.text(self.bytes.len())?.range;
                let relative_libid = self.text(self.bytes.len())?.range;
                let version_start = self.position;
                let major_version = self.u32()?;
                let minor_version = self.u16()?;
                ReferencePayload::Project {
                    absolute_libid,
                    relative_libid,
                    major_version,
                    minor_version,
                    version_range: version_start..self.position,
                }
            }
            _ => return Err(DirectoryError::Record { offset, id }),
        };
        Ok(ReferenceRecord {
            range: start..self.position,
            name,
            payload,
        })
    }

    fn reference_name(&mut self) -> Result<Option<ReferenceName>, DirectoryError> {
        if self.peek()? != 0x0016 {
            return Ok(None);
        }
        let (name, unicode_name) = self.pair(0x0016, self.bytes.len())?;
        Ok(Some(ReferenceName {
            name: name.range,
            unicode_name: unicode_name.range,
        }))
    }

    fn control(
        &mut self,
        original_libid: Option<std::ops::Range<usize>>,
    ) -> Result<ReferencePayload, DirectoryError> {
        self.marker(0x002f)?;
        let twiddled_libid = self.text(self.bytes.len())?.range;
        self.take(6)?;
        let extended_name = self.reference_name()?;
        self.take(2)?;
        self.take(4)?;
        let extended_libid = self.text(self.bytes.len())?.range;
        self.take(6)?;
        let guid_start = self.position;
        self.take(16)?;
        let original_type_library = guid_start..self.position;
        let cookie_start = self.position;
        let cookie = self.u32()?;
        Ok(ReferencePayload::Control {
            original_libid,
            twiddled_libid,
            extended_name,
            extended_libid,
            original_type_library,
            cookie,
            cookie_range: cookie_start..self.position,
        })
    }

    fn module(&mut self) -> Result<ModuleRecord<'a>, DirectoryError> {
        let start = self.position;
        self.id(0x0019)?;
        let name = self.text(self.bytes.len())?;
        let unicode_name = if self.peek()? == 0x0047 {
            self.id(0x0047)?;
            Some(self.unicode()?)
        } else {
            None
        };
        let (stream_name, unicode_stream_name) = self.pair(0x001a, self.bytes.len())?;
        self.pair(0x001c, self.bytes.len())?;
        self.header(0x0031, 4)?;
        let offset_start = self.position;
        let text_offset = self.u32()?;
        let text_offset_range = offset_start..self.position;
        self.header(0x001e, 4)?;
        self.take(4)?;
        self.header(0x002c, 2)?;
        self.take(2)?;
        let offset = self.position;
        let id = self.u16()?;
        let kind = match id {
            0x0021 => ModuleKind::Procedural,
            0x0022 => ModuleKind::Object,
            _ => return Err(DirectoryError::Record { offset, id }),
        };
        self.take(4)?;
        let read_only = self.optional_marker(0x0025)?;
        let private = self.optional_marker(0x0028)?;
        self.marker(0x002b)?;
        Ok(ModuleRecord {
            range: start..self.position,
            name,
            unicode_name,
            stream_name,
            unicode_stream_name,
            text_offset,
            text_offset_range,
            kind,
            read_only,
            private,
        })
    }
}
