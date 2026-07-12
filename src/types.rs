/// Top-level record tags (HPROF spec §2)
pub mod tags {
    pub const STRING_IN_UTF8: u8 = 0x01;
    pub const LOAD_CLASS: u8 = 0x02;
    #[allow(dead_code)]
    pub const STACK_FRAME: u8 = 0x04;
    #[allow(dead_code)]
    pub const STACK_TRACE: u8 = 0x05;
    #[allow(dead_code)]
    pub const START_THREAD: u8 = 0x0a;
    pub const HEAP_DUMP: u8 = 0x0c;
    pub const HEAP_DUMP_SEGMENT: u8 = 0x1c;
    pub const HEAP_DUMP_END: u8 = 0x2c;
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
    pub const CLASS_DUMP: u8 = 0x20;
    pub const INSTANCE_DUMP: u8 = 0x21;
    pub const OBJ_ARRAY_DUMP: u8 = 0x22;
    pub const PRIM_ARRAY_DUMP: u8 = 0x23;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
