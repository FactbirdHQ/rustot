use serde::{de::DeserializeOwned, Serialize};

use super::{Error, ShadowState};

pub trait ShadowDAO<S: Serialize + DeserializeOwned> {
    fn read(&mut self) -> Result<S, Error>;
    fn write(&mut self, state: &S) -> Result<(), Error>;
}

impl<S: Serialize + DeserializeOwned> ShadowDAO<S> for () {
    fn read(&mut self) -> Result<S, Error> {
        Err(Error::NoPersistence)
    }

    fn write(&mut self, _state: &S) -> Result<(), Error> {
        Err(Error::NoPersistence)
    }
}

pub struct EmbeddedStorageDAO<T: embedded_storage::Storage, const OFFSET: u32>(T);

impl<T, const OFFSET: u32> From<T> for EmbeddedStorageDAO<T, OFFSET>
where
    T: embedded_storage::Storage,
{
    fn from(v: T) -> Self {
        Self::new(v)
    }
}

impl<T, const OFFSET: u32> EmbeddedStorageDAO<T, OFFSET>
where
    T: embedded_storage::Storage,
{
    pub fn new(storage: T) -> Self {
        Self(storage)
    }
}

const U32_SIZE: usize = core::mem::size_of::<u32>();

impl<S, T, const OFFSET: u32> ShadowDAO<S> for EmbeddedStorageDAO<T, OFFSET>
where
    S: ShadowState + DeserializeOwned,
    T: embedded_storage::Storage,
    [(); S::MAX_PAYLOAD_SIZE + U32_SIZE]:,
{
    fn read(&mut self) -> Result<S, Error> {
        let buf = &mut [0u8; S::MAX_PAYLOAD_SIZE + U32_SIZE];

        self.0.read(OFFSET, buf).map_err(|_| Error::DaoRead)?;

        match buf[..U32_SIZE].try_into() {
            Ok(len_bytes) => {
                let len = u32::from_le_bytes(len_bytes);
                if len == 0xFFFFFFFF || len as usize + U32_SIZE > buf.len() {
                    return Err(Error::InvalidPayload);
                }

                Ok(
                    minicbor_serde::from_slice::<S>(&mut buf[U32_SIZE..len as usize + U32_SIZE])
                        .map_err(|_| Error::InvalidPayload)?,
                )
            }
            _ => Err(Error::InvalidPayload),
        }
    }

    fn write(&mut self, state: &S) -> Result<(), Error> {
        assert!(S::MAX_PAYLOAD_SIZE <= self.0.capacity() - OFFSET as usize);

        let buf = &mut [0u8; S::MAX_PAYLOAD_SIZE + U32_SIZE];

        let mut serializer = minicbor_serde::Serializer::new(minicbor::encode::write::Cursor::new(
            &mut buf[U32_SIZE..],
        ));

        state
            .serialize(&mut serializer)
            .map_err(|_| Error::InvalidPayload)?;
        let len = serializer.into_encoder().writer().position();

        if len > S::MAX_PAYLOAD_SIZE {
            return Err(Error::Overflow);
        }

        buf[..U32_SIZE].copy_from_slice(&(len as u32).to_le_bytes());

        self.0
            .write(OFFSET, &buf[..len + U32_SIZE])
            .map_err(|_| Error::DaoWrite)?;

        debug!("Wrote {} bytes to DAO @ {}", len + U32_SIZE, OFFSET);

        Ok(())
    }
}

#[cfg(any(feature = "std", test))]
pub struct StdIODAO<T: std::io::Write + std::io::Read>(pub(crate) T);

#[cfg(any(feature = "std", test))]
impl<T> From<T> for StdIODAO<T>
where
    T: std::io::Write + std::io::Read,
{
    fn from(v: T) -> Self {
        Self::new(v)
    }
}

#[cfg(any(feature = "std", test))]
impl<T> StdIODAO<T>
where
    T: std::io::Write + std::io::Read,
{
    pub fn new(storage: T) -> Self {
        Self(storage)
    }
}

#[cfg(any(feature = "std", test))]
impl<S, T> ShadowDAO<S> for StdIODAO<T>
where
    S: ShadowState + DeserializeOwned,
    T: std::io::Write + std::io::Read,
    [(); S::MAX_PAYLOAD_SIZE]:,
{
    fn read(&mut self) -> Result<S, Error> {
        let bytes = &mut [0u8; S::MAX_PAYLOAD_SIZE];

        self.0.read(bytes).map_err(|_| Error::DaoRead)?;
        let (shadow, _) = serde_json_core::from_slice(bytes).map_err(|_| Error::InvalidPayload)?;
        Ok(shadow)
    }

    fn write(&mut self, state: &S) -> Result<(), Error> {
        let bytes = serde_json_core::to_vec::<_, { S::MAX_PAYLOAD_SIZE }>(state)
            .map_err(|_| Error::Overflow)?;

        self.0.write(&bytes).map_err(|_| Error::DaoWrite)?;
        Ok(())
    }
}
