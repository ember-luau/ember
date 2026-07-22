use serde::Deserialize;

#[derive(Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
    pub _url: String,
    pub _id: i32,
}

#[derive(Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}
