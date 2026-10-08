/// Server configuration, loaded from environment variables with defaults.
pub struct Config {
    pub port: u16,
    pub api_key: String,
    pub base_url: String,
}

impl Config {
    pub fn from_env() -> Self {
        let port = std::env::var("MOCK_TRANSLATE_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(9090);

        let api_key =
            std::env::var("MOCK_TRANSLATE_API_KEY").unwrap_or_else(|_| BEARER_KEY.to_string());

        let base_url = std::env::var("MOCK_TRANSLATE_BASE_URL")
            .unwrap_or_else(|_| format!("http://127.0.0.1:{}", port));

        Self {
            port,
            api_key,
            base_url,
        }
    }
}

pub fn oauth_token_ttl_seconds() -> u64 {
    std::env::var("MOCK_TOKEN_TTL_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600)
}

// ---------------------------------------------------------------------------
// Shared test credentials (hardcoded for mock testing)
// ---------------------------------------------------------------------------

pub const BEARER_KEY: &str = "mock-translate-dev-key-2026";

pub const BAIDU_APPID: &str = "mock-appid-001";
pub const BAIDU_SECRET: &str = "mock-secret-baidu";

pub const YOUDAO_APP_KEY: &str = "mock-appkey-youdao";
pub const YOUDAO_APP_SECRET: &str = "mock-secret-youdao";

pub const HMAC_API_KEY: &str = "mock-key-hmac";
pub const HMAC_SECRET: &str = "mock-secret-hmac";

pub const TC3_SECRET_ID: &str = "mock-tc-id";
pub const TC3_SECRET_KEY: &str = "mock-tc-secret";

pub const AWS_ACCESS_KEY: &str = "mock-aws-key";
pub const AWS_SECRET_KEY: &str = "mock-aws-secret";

pub const VOLC_ACCESS_KEY: &str = "mock-volc-key";
pub const VOLC_SECRET_KEY: &str = "mock-volc-secret";

#[allow(dead_code)] // used in tests
pub const ALI_ACCESS_KEY: &str = "mock-ali-key";
pub const ALI_SECRET_KEY: &str = "mock-ali-secret";

pub const AZURE_SUB_KEY: &str = "mock-azure-sub-key";

pub const KAKAO_KEY: &str = "mock-kakao-key";

/// DeepL API Auth Key (header: `Authorization: DeepL-Auth-Key <key>`).
pub const DEEPL_AUTH_KEY: &str = "mock-deepl-auth-key";

/// Google Cloud Translation API key (JSON body `key` or `?key=`).
pub const GOOGLE_API_KEY: &str = "mock-google-api-key";

/// Google Cloud OAuth access token (Translation Advanced v3 / login flow).
pub const GOOGLE_ACCESS_TOKEN: &str = "mock-google-oauth-access-token";

/// Azure Entra / OAuth access token for Translator when not using subscription key.
pub const AZURE_ACCESS_TOKEN: &str = "mock-azure-oauth-access-token";

/// Naver Papago client credentials (`X-Naver-Client-Id` / `X-Naver-Client-Secret`).
pub const PAPAGO_CLIENT_ID: &str = "mock-papago-client-id";
pub const PAPAGO_CLIENT_SECRET: &str = "mock-papago-client-secret";

/// Yandex Translate API key (form/query `key`).
pub const YANDEX_API_KEY: &str = "mock-yandex-api-key";

/// iFlytek (科大讯飞) V1 mock credentials.
/// APP_ID identifies the app; API_SECRET is used in X-CheckSum HMAC.
pub const IFLYTEK_APP_ID: &str = "mock-iflytek-appid";
pub const IFLYTEK_API_SECRET: &str = "mock-iflytek-apisecret";

/// Niutrans (小牛翻译) V2 mock credentials.
pub const NIUTRANS_API_KEY: &str = "mock-niutrans-apikey";

pub const OAUTH_CLIENT_ID: &str = "mock-oauth-client";
pub const OAUTH_CLIENT_SECRET: &str = "mock-oauth-secret";

#[allow(dead_code)] // used in tests for roundtrip signing
/// RSA-2048 test private key (PKCS#8 PEM) for JWT/RSA signing.
pub const JWT_RSA_PRIVATE_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDDjJF4D4ifVZ1y
LZzAB/K6M1QtVqBHIRVnSAS5lAZxGfNvARo8agDkOUc77t7uQ2iSMAEzpvzevYPI
ksihHUPWaO99YB/Ubqrm2pjDlTSf7+FeU8H5YBgufAV6aLM3dSxegjVnVGqZBz94
HJkmrwjDzJZstxOQar1bR9pffV3aL/VSeZ9jsDjSqcxORkgBKA6PU+Rky9Un9s9B
gILtF2/o0k9eyj0VrrpzB2K6lTIlGdc9HpKaCuPlP/TfiZ1vzDppc+VRYIZwth4r
KhafDb8jprrR3fFfGkjEm77PsPuqSYCd5JqC+cNx3UjHLUsh8AlPgu0Z9APBb7gD
fOpbK2lDAgMBAAECggEAX8GJylzw7fUisDKdu3so5P0Vj04IFkyhCn49shJGRUQL
7KOBkWvnG9rggvyptcrUfiF7tStkUD5FNgaKsDqAaZHhFGfprkCy93C7tapuppL0
rRgeuFrR/JFGrXZvrAx4uZ7RP9Qajuz5q3t1k3fKtvaMAphzwBhEuVoqGPBu8NQI
CFf66dijyJzyBIQMtBywO+RhBgclG2wBkxMFP73phNiGK8wsaYwYiFuFWunr15Y5
nrQIcO1K2rTBz7SciF4HuFos62nft+8fZRDe1ATcT5XITob/z66bvlgdxN9qnysG
l2tAtibNwjzfUk/nvUFrzkan90jQziH8YfA5YzmIoQKBgQDjt3gK+bv1ydPmIzSy
RM4KS/QTPNGn2kw1IleyxiF9dS+rhez6kgCHfS3+/webkmTCTjiEuOepxFa+osKv
81Tx9k3otbtZm/bPosUhg0/+LGrMoi19+OCWSyy1TQV+sWZA/NtAcKxs3bWssATa
lMSW0f85GTqeZShYaPZAUt42ZwKBgQDb1kqn1NmzgEtVGgNgQZblFYyTtqtvLGi8
RVlPorUpymCpXVxRHVi7baoOU6I1+Yf8Wz71HGjJ8GncoqQMz/MN8WTufeqVH+6y
Jh1jWCVjoWqEcJig6TiuQT6RTdosivNZIttGd/c38EC9skUsMdbJQFOho91rbnG6
QW7uCIiUxQKBgGb9zvxf0SQsgXtABxBt0CaWDbE5u7VIJVmOy81ekT0V6FaSfwkh
Sb393vTK0d0KDCpZiHR20MhWtckJAjbkPlIlTT1oHHE9+hYVD0XGB0L1QeSFoT+t
dZ5kJ7dkO4z+nTndYyi1DTc723RcNAXMbVGtbtqbT4jeRa4e4ula0YgXAoGAbfh0
wCAqBzFWfevVZIFrCo4NFJd8itCcVBIc60lpds5WLGMXmzwi9x+UwjX0HywEaqj6
hYMAqIQrcMOrbP1ZiNAIvYUfpBmlPljyuo+NpJlKv5XTxCrmv8TDl8xqJ34a3awi
JM4+TS1SNZLIJ6OG3oXfUCy4xtUo7xNseoaqTEECgYEAuV2WRcPSC899HZxTcP/l
A1jwhywZaN84qGaqE43/IlRcsiNVoITapk8tLeekzDQu6a8lh1pIHq/etVaj+EcN
6teuEQQQ1/uIXCCqHlKSD6/Eb8oJ3BlCAWd1wImolPciAiWWjTe5ibwc2/9X5zoo
ih0RgWqemHgHI7+Mdbcx9xQ=
-----END PRIVATE KEY-----";

/// RSA-2048 test public key (SPKI PEM) for JWT/RSA signature verification.
pub const JWT_RSA_PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAw4yReA+In1Wdci2cwAfy
ujNULVagRyEVZ0gEuZQGcRnzbwEaPGoA5DlHO+7e7kNokjABM6b83r2DyJLIoR1D
1mjvfWAf1G6q5tqYw5U0n+/hXlPB+WAYLnwFemizN3UsXoI1Z1RqmQc/eByZJq8I
w8yWbLcTkGq9W0faX31d2i/1UnmfY7A40qnMTkZIASgOj1PkZMvVJ/bPQYCC7Rdv
6NJPXso9Fa66cwdiupUyJRnXPR6Smgrj5T/034mdb8w6aXPlUWCGcLYeKyoWnw2/
I6a60d3xXxpIxJu+z7D7qkmAneSagvnDcd1Ixy1LIfAJT4LtGfQDwW+4A3zqWytp
QwIDAQAB
-----END PUBLIC KEY-----";

/// JWT signing key for OAuth tokens (HS256).
pub const OAUTH_JWT_SECRET: &str = "mock-oauth-jwt-secret-for-testing-only-2026";
