use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::{Aead, Payload}};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use wasm_bindgen::prelude::*;

const HEADER: &[u8] = b"RUXCAS\x01";
const DOMAIN: &[u8] = b"ruxredock-convergent-aes256gcm-v1";

#[wasm_bindgen]
pub fn hash(data: &[u8]) -> String { hex::encode(Sha256::digest(data)) }

fn material(fingerprint: &[u8]) -> ([u8;32],[u8;12],Vec<u8>) {
    let mut key = [0;32];
    Hkdf::<Sha256>::new(Some(DOMAIN),fingerprint).expand(b"chunk-key",&mut key).unwrap();
    let digest = Sha256::digest([DOMAIN,b"nonce",fingerprint].concat());
    let mut nonce = [0;12]; nonce.copy_from_slice(&digest[..12]);
    (key,nonce,[HEADER,fingerprint].concat())
}

#[wasm_bindgen]
pub fn encrypt(data: &[u8]) -> Vec<u8> {
    let (key,nonce,aad) = material(&Sha256::digest(data));
    let encrypted = Aes256Gcm::new_from_slice(&key).unwrap().encrypt(Nonce::from_slice(&nonce),Payload {msg:data,aad:&aad}).unwrap();
    [HEADER,&encrypted].concat()
}

fn open_chunk(payload: &[u8],fingerprint: &str,object_id: &str) -> Result<Vec<u8>,String> {
    if hash(payload) != object_id || !payload.starts_with(HEADER) { return Err("Ciphertext integrity check failed".into()); }
    let fingerprint = hex::decode(fingerprint).map_err(|_| "Invalid fingerprint")?;
    if fingerprint.len()!=32 { return Err("Invalid fingerprint".into()); }
    let (key,nonce,aad) = material(&fingerprint);
    let result = Aes256Gcm::new_from_slice(&key).unwrap().decrypt(Nonce::from_slice(&nonce),Payload{msg:&payload[HEADER.len()..],aad:&aad})
        .map_err(|_| "Chunk authentication failed")?;
    if Sha256::digest(&result).as_slice() != fingerprint { return Err("Plaintext integrity check failed".into()); }
    Ok(result)
}

#[wasm_bindgen]
pub fn decrypt(payload: &[u8],fingerprint: &str,object_id: &str) -> Result<Vec<u8>,JsValue> {
    open_chunk(payload,fingerprint,object_id).map_err(|e|JsValue::from_str(&e))
}

/// Incremental file hashing avoids retaining a whole file in browser memory.
#[wasm_bindgen]
pub struct FileHasher(Sha256);
#[wasm_bindgen]
impl FileHasher {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self { Self(Sha256::new()) }
    pub fn update(&mut self,data: &[u8]) { self.0.update(data); }
    pub fn finish(&self) -> String {hex::encode(self.0.clone().finalize())}
}
impl Default for FileHasher {fn default()->Self {Self::new()}}

#[wasm_bindgen]
pub struct Chunker {
    table: [u64;256], window: Vec<u8>, cursor: usize, filled: usize,
    rolling: u64, length: usize, min: usize, average: usize, max: usize,
}
#[wasm_bindgen]
impl Chunker {
    #[wasm_bindgen(constructor)]
    pub fn new(min: usize,average: usize,max: usize,window: usize) -> Result<Chunker,JsValue> {
        if window==0 || window>min || min>average || average>max || max>1048576 || !average.is_power_of_two() {
            return Err(JsValue::from_str("Invalid chunk configuration"));
        }
        let table = std::array::from_fn(|i| {
            let digest = Sha256::digest([b"ruxredock-buzhash-v1".as_slice(),&[i as u8]].concat());
            u64::from_be_bytes(digest[..8].try_into().unwrap())
        });
        Ok(Self {table,window:vec![0;window],cursor:0,filled:0,rolling:0,length:0,min,average,max})
    }
    /// Cut positions are relative to this input; the rolling window crosses cuts.
    pub fn feed(&mut self,data: &[u8]) -> Vec<u32> {
        let mut cuts = Vec::new();
        for (index,&byte) in data.iter().enumerate() {
            self.rolling = self.rolling.rotate_left(1) ^ self.table[byte as usize];
            if self.filled==self.window.len() {
                self.rolling ^= self.table[self.window[self.cursor] as usize].rotate_left((self.window.len()%64) as u32);
            } else {self.filled+=1;}
            self.window[self.cursor]=byte;
            self.cursor=(self.cursor+1)%self.window.len();
            self.length+=1;
            if self.length>=self.max || (self.length>=self.min && self.rolling & (self.average as u64-1)==0) {
                cuts.push((index+1) as u32); self.length=0;
            }
        }
        cuts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn convergence_authentication_and_round_trip() {
        let data=b"local mock payload object\0".repeat(100);
        let payload=encrypt(&data);
        assert_eq!(payload,encrypt(&data));
        assert_eq!(open_chunk(&payload,&hash(&data),&hash(&payload)).unwrap(),data);
        let mut damaged=payload.clone(); *damaged.last_mut().unwrap()^=1;
        assert!(open_chunk(&damaged,&hash(&data),&hash(&damaged)).is_err());
    }
    #[test]
    fn read_boundaries_do_not_change_chunk_boundaries() {
        let data:Vec<u8>=(0..200000).map(|i|Sha256::digest((i as u32).to_le_bytes())[0]).collect();
        let whole=Chunker::new(2048,8192,32768,64).unwrap().feed(&data);
        let mut chunker=Chunker::new(2048,8192,32768,64).unwrap();
        let split:Vec<_>=data.chunks(137).enumerate().flat_map(|(i,part)|chunker.feed(part).into_iter().map(move |cut|cut+(i*137) as u32)).collect();
        assert_eq!(whole,split);
    }
}
