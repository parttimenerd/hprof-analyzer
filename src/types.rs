//! HPROF wire-format vocabulary: the record-tag byte constants and the
//! primitive-type enum shared across the parser. These name the raw tag/type
//! codes read off the dump stream so the rest of the pipeline can match on
//! meaning rather than magic numbers.

/// Top-level record tags (HPROF spec §2)
pub mod tags {
    pub const STRING_IN_UTF8: u8 = 0x01;
    pub const LOAD_CLASS: u8 = 0x02;
    #[allow(dead_code)]
    pub const UNLOAD_CLASS: u8 = 0x03;
    #[allow(dead_code)]
    pub const STACK_FRAME: u8 = 0x04;
    #[allow(dead_code)]
    pub const STACK_TRACE: u8 = 0x05;
    #[allow(dead_code)]
    pub const START_THREAD: u8 = 0x0a;
    pub const HEAP_DUMP: u8 = 0x0c;
    pub const HEAP_DUMP_SEGMENT: u8 = 0x1c;
    pub const HEAP_DUMP_END: u8 = 0x2c;
    /// Custom marker record injected by `hprof-analyzer redact`. Tag 0xDE is
    /// unused in the HPROF spec; tools that don't know it skip it via the
    /// standard length-prefixed skip path.
    pub const REDACTED_MARKER: u8 = 0xDE;
}

/// Heap sub-record tags
pub mod heap {
    pub const ROOT_UNKNOWN: u8 = 0xff;
    pub const ROOT_JNI_GLOBAL: u8 = 0x01;
    pub const ROOT_JNI_LOCAL: u8 = 0x02;
    pub const ROOT_JAVA_FRAME: u8 = 0x03;
    pub const ROOT_NATIVE_STACK: u8 = 0x04;
    pub const ROOT_STICKY_CLASS: u8 = 0x05;
    pub const ROOT_THREAD_BLOCK: u8 = 0x06;
    pub const ROOT_MONITOR_USED: u8 = 0x07;
    pub const ROOT_THREAD_OBJ: u8 = 0x08;
    /// Wire: `id`. Emitted by Android ART as an explicit root for interned strings.
    pub const ROOT_INTERNED_STRING: u8 = 0x89;
    /// Wire: `id`. Android ART root for objects held by the debugger.
    pub const ROOT_DEBUGGER: u8 = 0x8b;
    /// Wire: `id`. Android ART root for VM-internal references.
    pub const ROOT_VM_INTERNAL: u8 = 0x8d;
    /// Wire: `id` + `u4 thread_serial` + `u4 frame_num`. Android ART JNI monitor root.
    pub const ROOT_JNI_MONITOR: u8 = 0x8e;
    /// Wire: `id` + `u4 stack_serial` + `u4 count` + `u1 elem_type` (no element data).
    /// Android ART obsolete variant of PRIM_ARRAY_DUMP that omits the payload.
    pub const PRIM_ARRAY_NODATA_DUMP: u8 = 0xc3;
    /// Synthetic system-class root used internally (MAT addSystemClassRootsIfMissing).
    /// Also emitted by some non-HotSpot JVMs (e.g. IBM J9) as an explicit root.
    /// Wire: `id` only.
    pub const ROOT_SYSTEM_CLASS: u8 = 0x00;
    pub const CLASS_DUMP: u8 = 0x20;
    pub const INSTANCE_DUMP: u8 = 0x21;
    pub const OBJ_ARRAY_DUMP: u8 = 0x22;
    pub const PRIM_ARRAY_DUMP: u8 = 0x23;
    /// HEAP_DUMP_INFO: `u4 heap_id` + `id heap_name_string_id`. Carries no
    /// object/class data — consumed and skipped so sub-record stream stays aligned.
    pub const HEAP_DUMP_INFO: u8 = 0xfe;
}

/// A field/array element's primitive type, as encoded by HPROF type codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HprofType {
    Object,
    Boolean,
    Char,
    Float,
    Double,
    Byte,
    Short,
    Int,
    Long,
}

impl HprofType {
    /// Maps a raw HPROF type code to its type; `None` for unknown codes.
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            2 => Some(Self::Object),
            4 => Some(Self::Boolean),
            5 => Some(Self::Char),
            6 => Some(Self::Float),
            7 => Some(Self::Double),
            8 => Some(Self::Byte),
            9 => Some(Self::Short),
            10 => Some(Self::Int),
            11 => Some(Self::Long),
            _ => None,
        }
    }

    /// Returns 0 for Object (caller must use id_size separately)
    pub fn byte_size(self) -> usize {
        match self {
            Self::Object => 0,
            Self::Boolean | Self::Byte => 1,
            Self::Char | Self::Short => 2,
            Self::Float | Self::Int => 4,
            Self::Double | Self::Long => 8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_sizes() {
        assert_eq!(HprofType::Boolean.byte_size(), 1);
        assert_eq!(HprofType::Char.byte_size(), 2);
        assert_eq!(HprofType::Float.byte_size(), 4);
        assert_eq!(HprofType::Double.byte_size(), 8);
        assert_eq!(HprofType::Byte.byte_size(), 1);
        assert_eq!(HprofType::Short.byte_size(), 2);
        assert_eq!(HprofType::Int.byte_size(), 4);
        assert_eq!(HprofType::Long.byte_size(), 8);
    }

    #[test]
    fn type_from_code() {
        assert_eq!(HprofType::from_code(4), Some(HprofType::Boolean));
        assert_eq!(HprofType::from_code(2), Some(HprofType::Object));
        assert_eq!(HprofType::from_code(99), None);
    }
}
