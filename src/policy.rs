use std::{env,time::Duration};

#[derive(Clone)]
pub struct UploadPolicy {
    pub global: bool,
    pub minimum_response: Duration,
    pub bytes_per_second: u64,
}
impl Default for UploadPolicy {
    fn default()->Self {Self {global:false,minimum_response:Duration::from_millis(40),bytes_per_second:20*1024*1024}}
}
impl UploadPolicy {
    pub fn from_env()->Result<Self,Box<dyn std::error::Error>> {
        let global=match env::var("DEDUP_SCOPE").unwrap_or_else(|_|"tenant".into()).as_str() {
            "tenant"=>false,"global"=>true,_=>return Err("DEDUP_SCOPE must be tenant or global".into()),
        };
        let ms=env::var("MIN_RESPONSE_MS").unwrap_or_else(|_|"40".into()).parse::<u64>()?;
        let rate=env::var("SIMULATED_BYTES_PER_SECOND").unwrap_or_else(|_|"20971520".into()).parse::<u64>()?;
        if rate==0 {return Err("SIMULATED_BYTES_PER_SECOND must be positive".into());}
        Ok(Self {global,minimum_response:Duration::from_millis(ms),bytes_per_second:rate})
    }
}
