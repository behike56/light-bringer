use std::{env, fs, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use reqwest::{
    Certificate, Method, Url,
    blocking::Client,
    header::{ACCEPT, HeaderValue},
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

const AFFINITY: &str = "neo4j-cluster-affinity";

pub(super) struct QueryClient {
    http: Client,
    url: Url,
    user: String,
    password: String,
}

#[derive(Default, Deserialize)]
pub(super) struct Reply {
    data: Option<QueryData>,
    transaction: Option<TransactionInfo>,
    bookmarks: Option<Vec<String>>,
    #[serde(default)]
    errors: Vec<QueryError>,
}

#[derive(Deserialize)]
struct QueryData {
    values: Vec<Value>,
}

#[derive(Deserialize)]
struct TransactionInfo {
    id: String,
}

#[derive(Deserialize)]
struct QueryError {
    code: String,
}

pub(super) struct Transaction<'a> {
    client: &'a QueryClient,
    url: Url,
    affinity: Option<HeaderValue>,
    finished: bool,
}

impl QueryClient {
    pub(super) fn connect(url: Url, role: &str) -> Result<Self> {
        let credential = |key| {
            let name = format!("NEO4J_{role}_{key}");
            let value = env::var(&name).with_context(|| format!("{name} を設定してください"))?;
            ensure!(!value.is_empty(), "{name} を設定してください");
            Ok(value)
        };
        let user = credential("USER")?;
        let password = credential("PASSWORD")?;
        let mut http = Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120));
        if let Some(path) = env::var_os("NEO4J_CA_CERT") {
            let pem = fs::read(path).context("NEO4J_CA_CERT のファイルを開けません")?;
            for certificate in Certificate::from_pem_bundle(&pem)? {
                http = http.add_root_certificate(certificate);
            }
        }
        Ok(Self {
            http: http
                .build()
                .context("HTTPS クライアントを初期化できません")?,
            url,
            user,
            password,
        })
    }

    pub(super) fn query(&self, statement: &str, parameters: Value) -> Result<Reply> {
        let (reply, _) = self.request(
            Method::POST,
            self.url.clone(),
            None,
            &query(statement, parameters),
        )?;
        Ok(reply)
    }

    pub(super) fn begin(
        &self,
        statement: &str,
        parameters: Value,
    ) -> Result<(Transaction<'_>, Reply)> {
        let mut url = self.url.clone();
        url.path_segments_mut().expect("HTTPS URL").push("tx");
        let (reply, affinity) = self.request(
            Method::POST,
            url.clone(),
            None,
            &query(statement, parameters),
        )?;
        let id = &reply
            .transaction
            .as_ref()
            .context("Neo4j がトランザクション ID を返しませんでした")?
            .id;
        ensure!(
            !id.is_empty() && id != "." && id != "..",
            "不正なトランザクション ID です"
        );
        url.path_segments_mut().expect("HTTPS URL").push(id);
        Ok((
            Transaction {
                client: self,
                url,
                affinity,
                finished: false,
            },
            reply,
        ))
    }

    fn request(
        &self,
        method: Method,
        url: Url,
        affinity: Option<&HeaderValue>,
        body: &Value,
    ) -> Result<(Reply, Option<HeaderValue>)> {
        let rollback = method == Method::DELETE;
        let mut request = self
            .http
            .request(method, url)
            .basic_auth(&self.user, Some(&self.password))
            .header(ACCEPT, "application/json");
        if let Some(affinity) = affinity {
            request = request.header(AFFINITY, affinity);
        }
        let response = if rollback {
            request.timeout(Duration::from_secs(5)).send()
        } else {
            request.json(body).send()
        }
        .context("Neo4j Query API との HTTPS 通信に失敗しました")?;

        let status = response.status();
        if rollback && status.is_success() {
            return Ok((Reply::default(), None));
        }
        ensure!(
            status.as_u16() == 202,
            "Neo4j Query API が HTTP {status} を返しました"
        );
        let affinity = response.headers().get(AFFINITY).cloned();
        let reply: Reply = response
            .json()
            .context("Neo4j Query API の応答を読み込めません")?;
        if let Some(error) = reply.errors.first() {
            // Error messages may include data or Cypher parameters. Report only the status code.
            let code = &error.code;
            ensure!(
                code.starts_with("Neo.")
                    && code
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.'),
                "Neo4j がエラーを返しました"
            );
            bail!("Neo4j エラー（{code}）");
        }
        Ok((reply, affinity))
    }
}

impl Reply {
    pub(super) fn rows<T: DeserializeOwned>(self) -> Result<Vec<T>> {
        self.data
            .context("Neo4j Query API の応答に data がありません")?
            .values
            .into_iter()
            .map(|row| {
                serde_json::from_value(row).context("Neo4j の結果が想定した形式と異なります")
            })
            .collect()
    }

    pub(super) fn one<T: DeserializeOwned>(self) -> Result<T> {
        let mut rows = self.rows()?;
        ensure!(rows.len() == 1, "Neo4j から 1 行の応答が必要です");
        Ok(rows.remove(0))
    }
}

impl Transaction<'_> {
    pub(super) fn query(&self, statement: &str, parameters: Value) -> Result<Reply> {
        let (reply, _) = self.client.request(
            Method::POST,
            self.url.clone(),
            self.affinity.as_ref(),
            &query(statement, parameters),
        )?;
        Ok(reply)
    }

    pub(super) fn commit(mut self) -> Result<()> {
        let mut url = self.url.clone();
        url.path_segments_mut().expect("HTTPS URL").push("commit");
        let (reply, _) =
            self.client
                .request(Method::POST, url, self.affinity.as_ref(), &json!({}))?;
        ensure!(
            reply
                .bookmarks
                .is_some_and(|bookmarks| !bookmarks.is_empty()),
            "Neo4j のコミット応答を確認できません"
        );
        self.finished = true;
        Ok(())
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.client.request(
                Method::DELETE,
                self.url.clone(),
                self.affinity.as_ref(),
                &Value::Null,
            );
        }
    }
}

fn query(statement: &str, parameters: Value) -> Value {
    json!({ "statement": statement, "parameters": parameters })
}
