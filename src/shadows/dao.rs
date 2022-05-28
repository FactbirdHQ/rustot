use super::{Error, ShadowState, MAX_PAYLOAD_SIZE};

pub trait ShadowDAO {
    fn read<S: ShadowState>(&mut self) -> Result<S, Error>;
    fn write<S: ShadowState>(&mut self, state: &S) -> Result<(), Error>;
}

impl ShadowDAO for () {
    fn read<S: ShadowState>(&mut self) -> Result<S, Error> {
        Err(Error::NoPersistance)
    }

    fn write<S: ShadowState>(&mut self, _state: &S) -> Result<(), Error> {
        Err(Error::NoPersistance)
    }
}

pub struct EmbeddedStorageDAO<T: embedded_storage::Storage> {
    storage: T,
    offset: u32,
}

impl<T> EmbeddedStorageDAO<T>
where
    T: embedded_storage::Storage,
{
    pub fn new(storage: T, offset: u32) -> Self {
        Self { storage, offset }
    }
}

impl<T> ShadowDAO for EmbeddedStorageDAO<T>
where
    T: embedded_storage::Storage,
{
    fn read<S: ShadowState>(&mut self) -> Result<S, Error> {
        let bytes = &mut [0u8; MAX_PAYLOAD_SIZE];

        self.storage
            .read(self.offset, bytes)
            .map_err(|_| Error::DaoRead)?;
        serde_cbor::de::from_mut_slice(bytes).map_err(|_| Error::InvalidPayload)
    }

    fn write<S: ShadowState>(&mut self, state: &S) -> Result<(), Error> {
        assert!(MAX_PAYLOAD_SIZE <= self.storage.capacity() - self.offset as usize);

        let bytes = &mut [0u8; MAX_PAYLOAD_SIZE];

        let mut serializer =
            serde_cbor::ser::Serializer::new(serde_cbor::ser::SliceWrite::new(bytes));
        state
            .serialize(&mut serializer)
            .map_err(|_| Error::InvalidPayload)?;
        let len = serializer.into_inner().bytes_written();

        self.storage
            .write(self.offset, &bytes[..len])
            .map_err(|_| Error::DaoWrite)
    }
}

#[cfg(any(feature = "std", test))]
pub struct StdIODAO<T: std::io::Write + std::io::Read> {
    pub(crate) storage: T,
}

#[cfg(any(feature = "std", test))]
impl<T> StdIODAO<T>
where
    T: std::io::Write + std::io::Read,
{
    pub fn new(storage: T) -> Self {
        Self { storage }
    }
}

#[cfg(any(feature = "std", test))]
impl<T> ShadowDAO for StdIODAO<T>
where
    T: std::io::Write + std::io::Read,
{
    fn read<S: ShadowState>(&mut self) -> Result<S, Error> {
        let bytes = &mut [0u8; MAX_PAYLOAD_SIZE];

        self.storage.read(bytes).map_err(|_| Error::DaoRead)?;
        let (shadow, _) = serde_json_core::from_slice(bytes).map_err(|_| Error::InvalidPayload)?;
        Ok(shadow)
    }

    fn write<S: ShadowState>(&mut self, state: &S) -> Result<(), Error> {
        let bytes =
            serde_json_core::to_vec::<_, MAX_PAYLOAD_SIZE>(state).map_err(|_| Error::Overflow)?;

        self.storage.write(&bytes).map_err(|_| Error::DaoWrite)?;
        Ok(())
    }
}
