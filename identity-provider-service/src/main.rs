use anyhow::{bail, ensure};
use crypto_common::{base16_encode_string, SerdeDeserialize, SerdeSerialize, Versioned, VERSION_0};
use id::{
    constants::{ArCurve, IpPairing},
    ffi::AttributeKind,
    identity_provider::{
        create_initial_cdi, sign_identity_object, validate_request as ip_validate_request,
    },
    types::*,
};
use log::{error, info, warn};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{from_str, json, to_value};
use std::{
    collections::HashMap,
    convert::Infallible,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use structopt::StructOpt;
use warp::{http::StatusCode, hyper::header::LOCATION, Filter, Rejection, Reply};

type ExampleAttributeList = AttributeList<id::constants::BaseField, AttributeKind>;

#[derive(Deserialize)]
struct IdentityObjectRequest {
    #[serde(rename = "idObjectRequest")]
    id_object_request: Versioned<PreIdentityObject<IpPairing, ArCurve>>,
}

/// Holds the query parameters expected by the service.
#[derive(Deserialize)]
struct Input {
    /// The JSON serialized and URL encoded identity request
    /// object.
    /// The name 'state' is what is expected as a GET parameter name.
    #[serde(rename = "state")]
    state: String,
    /// The URI where the response will be returned.
    #[serde(rename = "redirect_uri")]
    redirect_uri: String,
}

/// JSON object that the wallet expects to be returned when polling for an
/// identity object.
#[derive(Serialize)]
struct IdentityTokenContainer {
    status: String,
    token:  serde_json::Value,
    detail: String,
}

/// Holds the information required to create the IdentityObject and forward to
/// the correct response URL that the wallet is expecting. Used for easier
/// passing between methods.
struct ValidatedRequest {
    /// The pre-identity-object contained in the initial request.
    request: PreIdentityObject<IpPairing, ArCurve>,
    /// The URI that the ID object should be returned to after we've done the
    /// verification of the user.
    redirect_uri: String,
}

/// Structure used to receive the correct command line arguments by using
/// StructOpt.
#[derive(Debug, StructOpt)]
struct IdentityProviderServiceConfiguration {
    #[structopt(
        long = "global-context",
        help = "File with global context.",
        env = "GLOBAL_CONTEXT",
        default_value = "global.json"
    )]
    global_context_file: PathBuf,
    #[structopt(
        long = "identity-provider",
        help = "File with the identity provider as JSON.",
        default_value = "identity_providers.json",
        env = "IDENTITY_PROVIDER"
    )]
    identity_provider_file: PathBuf,
    #[structopt(
        long = "anonymity-revokers",
        help = "File with the list of anonymity revokers as JSON.",
        default_value = "identity_providers.json",
        env = "ANONYMITY_REVOKERS"
    )]
    anonymity_revokers_file: PathBuf,
    #[structopt(
        long = "port",
        default_value = "8100",
        help = "Port on which the server will listen on.",
        env = "IDENTITY_PROVIDER_SERVICE_PORT"
    )]
    port: u16,
    #[structopt(
        long = "retrieve-base",
        help = "Base URL where the wallet can retrieve the identity object.",
        env = "RETRIEVE_BASE"
    )]
    retrieve_url: url::Url,
    #[structopt(
        long = "id-verification-url",
        help = "URL of the identity verifier.",
        default_value = "http://localhost:8101/api/verify",
        env = "ID_VERIFICATION_URL"
    )]
    id_verification_url: url::Url,
    #[structopt(
        long = "wallet-proxy-base",
        help = "URL of the wallet-proxy.",
        env = "WALLET_PROXY_BASE"
    )]
    wallet_proxy_base: url::Url,
}
/// The state the server maintains in-between the requests.
struct ServerConfig {
    ip_data:               IpData<IpPairing>,
    global:                GlobalContext<ArCurve>,
    ars:                   ArInfos<ArCurve>,
    id_verification_url:   url::Url,
    retrieve_url:          url::Url,
    submit_credential_url: url::Url,
}

fn load_server_config(
    config: &IdentityProviderServiceConfiguration,
) -> anyhow::Result<ServerConfig> {
    let ip_data_contents = fs::read_to_string(&config.identity_provider_file)?;
    let ar_info_contents = fs::read_to_string(&config.anonymity_revokers_file)?;
    let global_context_contents = fs::read_to_string(&config.global_context_file)?;
    let ip_data = from_str(&ip_data_contents)?;
    let versioned_global = from_str::<Versioned<_>>(&global_context_contents)?;
    let versioned_ar_infos = from_str::<Versioned<_>>(&ar_info_contents)?;
    ensure!(
        versioned_global.version == VERSION_0,
        "Unsupported global parameters version."
    );
    ensure!(
        versioned_ar_infos.version == VERSION_0,
        "Unsupported anonymity revokers version."
    );
    let mut submit_credential_url = config.wallet_proxy_base.clone();
    submit_credential_url.set_path("v0/submitCredential/");
    Ok(ServerConfig {
        ip_data,
        global: versioned_global.value,
        ars: versioned_ar_infos.value,
        id_verification_url: config.id_verification_url.clone(),
        retrieve_url: config.retrieve_url.clone(),
        submit_credential_url,
    })
}

/// A mockup of a database to store all the data.
/// In production this would be a real database.
#[derive(Clone)]
struct DB {
    /// Root directory where all the data is stored.
    root: std::path::PathBuf,
    /// Root of the backup directory where we store "deleted" files.
    backup_root: std::path::PathBuf,
    /// And a hashmap of pending entries. Pending entries are also stored in the
    /// filesystem, but we cache them here since they have to be accessed
    /// often. We put it behind a mutex to sync all accesses, to the hashmap
    /// as well as to the filesystem, which is implicit. In a real database
    /// this would be done differently.
    pending: Arc<Mutex<HashMap<String, PendingEntry>>>,
}

#[derive(SerdeSerialize, SerdeDeserialize, Clone)]
#[serde(rename_all = "lowercase")]
enum PendingStatus {
    Submitted {
        submission_id: String,
        status:        SubmissionStatus,
    },
    CouldNotSubmit,
}

#[derive(SerdeDeserialize, SerdeSerialize)]
#[serde(rename_all = "camelCase")]
/// Successful response from the wallet proxy.
struct InitialAccountReponse {
    submission_id: String,
}

#[derive(SerdeSerialize, SerdeDeserialize, Clone)]
struct PendingEntry {
    pub status: PendingStatus,
    pub value:  serde_json::Value,
}

impl DB {
    pub fn new(root: std::path::PathBuf, backup_root: std::path::PathBuf) -> anyhow::Result<Self> {
        // Create the 'database' directories for storing IdentityObjects and
        // AnonymityRevocationRecords.
        fs::create_dir_all(root.join("revocation"))?;
        fs::create_dir_all(root.join("identity"))?;
        fs::create_dir_all(root.join("pending"))?;
        let mut hm = HashMap::new();
        for file in fs::read_dir(root.join("pending"))? {
            if let Ok(file) = file {
                if file.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                    let name = file
                        .file_name()
                        .into_string()
                        .expect("Base16 strings are valid strings.");
                    let contents = fs::read_to_string(file.path())?;
                    let entry = from_str::<PendingEntry>(&contents)?;
                    hm.insert(name, entry);
                }
            }
        }
        let pending = Arc::new(Mutex::new(hm));
        Ok(Self {
            root,
            backup_root,
            pending,
        })
    }

    pub fn write_revocation_record(
        &self,
        key: &str,
        record: &AnonymityRevocationRecord<ArCurve>,
    ) -> anyhow::Result<()> {
        let _lock = self
            .pending
            .lock()
            .expect("Cannot acquire a lock, which means something is very wrong.");
        {
            // FIXME: We should be careful to not overwrite here.
            let file = std::fs::File::create(self.root.join("revocation").join(key))?;
            serde_json::to_writer(file, record)?;
        } // close the file
          // and now drop the lock as well.
        Ok(())
    }

    pub fn write_identity_object(
        &self,
        key: &str,
        obj: &Versioned<IdentityObject<IpPairing, ArCurve, AttributeKind>>,
    ) -> anyhow::Result<()> {
        let _lock = self
            .pending
            .lock()
            .expect("Cannot acquire a lock, which means something is very wrong.");
        {
            let file = std::fs::File::create(self.root.join("identity").join(key))?;
            let stored_obj = json!({
                "identityObject": obj,
                "accountAddress": AccountAddress::new(&obj.value.pre_identity_object.pub_info_for_ip.reg_id)
            });
            serde_json::to_writer(file, &stored_obj)?;
        }
        Ok(())
    }

    pub fn read_identity_object(&self, key: &str) -> anyhow::Result<serde_json::Value> {
        // ensure the key is valid base16 characters, which also ensures we are only
        // reading in the subdirectory FIXME: This is an inefficient way of
        // doing it.
        if hex::decode(key).is_err() {
            bail!("Invalid key.")
        }

        let contents = {
            let _lock = self
                .pending
                .lock()
                .expect("Cannot acquire a lock, which means something is very wrong.");
            fs::read_to_string(self.root.join("identity").join(key))?
        }; // drop the lock at this point
           // It is more efficient to read the whole thing, and then deserialize
        Ok(from_str::<serde_json::Value>(&contents)?)
    }

    pub fn write_pending(
        &self,
        key: &str,
        status: PendingStatus,
        value: serde_json::Value,
    ) -> anyhow::Result<()> {
        let mut lock = self
            .pending
            .lock()
            .expect("Cannot acquire a lock, which means something is very wrong.");
        {
            let file = std::fs::File::create(self.root.join("pending").join(key))?;
            let value = PendingEntry { status, value };
            serde_json::to_writer(file, &value)?;
            lock.insert(key.to_string(), value);
        }
        Ok(())
    }

    pub fn mark_finalized(&self, key: &str) {
        let mut lock = self.pending.lock().unwrap();
        lock.remove(key);
        let pending_path = self.root.join("pending").join(key);
        std::fs::remove_file(pending_path).unwrap();
    }

    pub fn delete_all(&self, key: &str) {
        let mut lock = self.pending.lock().unwrap();
        let ar_record_path = self.root.join("revocation").join(key);
        let id_path = self.root.join("identity").join(key);
        let pending_path = self.root.join("pending").join(key);

        std::fs::rename(
            ar_record_path,
            self.backup_root.join("revocation").join(key),
        )
        .unwrap();
        std::fs::rename(id_path, self.backup_root.join("identity").join(key)).unwrap();
        std::fs::remove_file(pending_path).unwrap();
        lock.remove(key);
    }

    pub fn is_pending(&self, key: &str) -> bool { self.pending.lock().unwrap().get(key).is_some() }
}

#[derive(SerdeSerialize, SerdeDeserialize, Clone)]
#[serde(rename_all = "lowercase")]
enum SubmissionStatus {
    Absent,
    Received,
    Committed,
    Finalized,
}

#[derive(SerdeSerialize, SerdeDeserialize)]
#[serde(rename_all = "camelCase")]
/// The part of the response we care about. Since the transaction
/// will either be in a block, or not, and if it is, then the account will have
/// been created.
struct SubmissionStatusResponse {
    status: SubmissionStatus,
}

async fn followup(
    client: Client,
    db: DB,
    submission_url: url::Url,
    mut query_url_base: url::Url,
    key: String,
) {
    loop {
        let v = {
            let hm = db.pending.lock().unwrap();
            hm.get(&key).cloned()
        }; // release lock
        if let Some(v) = v {
            match &v.status {
                PendingStatus::CouldNotSubmit => {
                    match submit_account_creation(&client, submission_url.clone(), &v.value).await {
                        Ok(new_status) => {
                            let mut hm = db.pending.lock().unwrap();
                            if let Some(point) = hm.get_mut(&key) {
                                point.status = new_status;
                            } else {
                                break;
                            }
                        }
                        Err(_) => {
                            db.delete_all(&key);
                            warn!("Account creation transaction rejected.");
                            break;
                        }
                    }
                }
                PendingStatus::Submitted { submission_id, .. } => {
                    query_url_base.set_path(&format!("v0/submissionStatus/{}", submission_id));
                    match client.get(query_url_base.clone()).send().await {
                        Ok(response) => {
                            match response.status() {
                                StatusCode::OK => {
                                    match response.json::<SubmissionStatusResponse>().await {
                                        Ok(ss) => {
                                            match ss.status {
                                                SubmissionStatus::Finalized => {
                                                    db.mark_finalized(&key);
                                                    info!(
                                                        "Account creation transaction finalized."
                                                    );
                                                    break;
                                                }
                                                SubmissionStatus::Absent => error!(
                                                    "An account creation transaction has gone \
                                                     missing. This indicates a configuration \
                                                     error."
                                                ),
                                                SubmissionStatus::Received => {} /* do nothing, wait for the next iteration */
                                                SubmissionStatus::Committed => {} /* do nothing, wait for the next iteration */
                                            }
                                        }
                                        Err(e) => error!(
                                            "Received unexpected response when querying \
                                             submission status: {}.",
                                            e
                                        ),
                                    }
                                }
                                other => error!(
                                    "Received unexpected response when querying submission \
                                     status: {}.",
                                    other
                                ),
                            }
                        }
                        Err(e) => {
                            error!(
                                "Could not query submission status for {} due to: {}.",
                                key, e
                            );
                            // and do nothing
                        }
                    }
                }
            }
            std::thread::sleep(Duration::new(5, 0));
        } else {
            break;
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let app = IdentityProviderServiceConfiguration::clap()
        .setting(clap::AppSettings::ArgRequiredElseHelp)
        .global_setting(clap::AppSettings::ColoredHelp);
    let matches = app.get_matches();
    let opt = Arc::new(IdentityProviderServiceConfiguration::from_clap(&matches));

    info!("Reading the provided IP, AR and global context configurations.");

    let server_config = Arc::new(load_server_config(&opt)?);

    // Client used to make HTTP requests to both the id verifier,
    // as well as to submit the initial account creation.
    // We reuse it between requests since it is expensive to create.
    let client = Client::new();

    // Create the 'database' directories for storing IdentityObjects and
    // AnonymityRevocationRecords.
    let db = DB::new(
        std::path::Path::new("database").to_path_buf(),
        std::path::Path::new("database-deleted").to_path_buf(),
    )?;
    info!("Configurations have been loaded successfully.");

    let retrieval_db = db.clone();

    let retrieve_identity = warp::get()
        .and(warp::path!("api" / "identity" / String))
        .map(move |id_cred_pub: String| {
            info!("Queried for receiving identity: {}", id_cred_pub);
            if retrieval_db.is_pending(&id_cred_pub) {
                info!("Identity object is pending.");
                let identity_token_container = IdentityTokenContainer {
                    status: "pending".to_string(),
                    detail: "Pending initial account creation.".to_string(),
                    token:  serde_json::Value::Null,
                };
                warp::reply::json(&identity_token_container)
            } else {
                match retrieval_db.read_identity_object(&id_cred_pub) {
                    Ok(identity_object) => {
                        info!("Identity object found");

                        let identity_token_container = IdentityTokenContainer {
                            status: "done".to_string(),
                            token:  identity_object,
                            detail: "".to_string(),
                        };
                        warp::reply::json(&identity_token_container)
                    }
                    Err(_e) => {
                        info!("Identity object does not exist or the request is malformed.");
                        let error_identity_token_container = IdentityTokenContainer {
                            status: "error".to_string(),
                            detail: "Identity object does not exist".to_string(),
                            token:  serde_json::Value::Null,
                        };
                        warp::reply::json(&error_identity_token_container)
                    }
                }
            }
        });

    let server_config_validate = Arc::clone(&server_config);
    let create_identity = warp::get()
        .and(warp::path!("api" / "identity"))
        .and(extract_and_validate_request(server_config_validate))
        .and_then(move |idi| {
            create_signed_identity_object(
                Arc::clone(&server_config),
                db.clone(),
                client.clone(),
                idi,
            )
        });

    info!("Booting up HTTP server. Listening on port {}.", opt.port);
    let server = create_identity
        .or(retrieve_identity)
        .recover(handle_rejection);
    warp::serve(server).run(([0, 0, 0, 0], opt.port)).await;
    Ok(())
}

macro_rules! ok_or_500 (
    ($e: expr, $s: expr) => {
        if $e.is_err() {
            error!($s);
            return Err(warp::reject::custom(IdRequestRejection::InternalError))
        }
    };
);

/// Submit an account creation transaction. Return Ok if either the submission
/// was successful or if it failed due to reasons unrelated to the request
/// itself, e.g., we could not reach the server. Return Err(_) if the submission
/// is malformed for some reason.
async fn submit_account_creation(
    client: &Client,
    url: url::Url,
    submission: &serde_json::Value,
) -> Result<PendingStatus, String> {
    // Submit and wait for the submission ID.
    match client.put(url).json(submission).send().await {
        Ok(response) => {
            match response.status() {
                StatusCode::BAD_GATEWAY => {
                    // internal server error, retry later.
                    Ok(PendingStatus::CouldNotSubmit)
                }
                StatusCode::BAD_REQUEST => {
                    Err("Failed validation of the reuse of malformed initial account.".to_string())
                }
                StatusCode::OK => match response.json::<InitialAccountReponse>().await {
                    Ok(v) => {
                        let initial_status = PendingStatus::Submitted {
                            submission_id: v.submission_id,
                            status:        SubmissionStatus::Received,
                        };
                        Ok(initial_status)
                    }
                    Err(_) => Ok(PendingStatus::CouldNotSubmit),
                },
                other => {
                    error!("Unexpected response from the Wallet Proxy: {}", other);
                    // internal server error, retry later.
                    Ok(PendingStatus::CouldNotSubmit)
                }
            }
        }
        Err(e) => {
            // This almost certainly means we could not reach the server, or the server is
            // configured wrong. This should be considered an internal error and
            // we must retry.
            error!("Could not reach the wallet proxy due to: {}", e);
            Ok(PendingStatus::CouldNotSubmit)
        }
    }
}

#[derive(Debug)]
enum IdRequestRejection {
    CouldNotParse,
    UnsupportedVersion,
    InvalidProofs,
    IdVerifierFailure,
    InternalError,
    ReuseOfRegId,
}

impl warp::reject::Reject for IdRequestRejection {}

async fn handle_rejection(err: Rejection) -> Result<impl warp::Reply, Infallible> {
    if err.is_not_found() {
        let code = StatusCode::NOT_FOUND;
        let message = "Not found.";
        Ok(warp::reply::with_status(message, code))
    } else if let Some(IdRequestRejection::CouldNotParse) = err.find() {
        let code = StatusCode::BAD_REQUEST;
        let message = "Could not parse the request.";
        Ok(warp::reply::with_status(message, code))
    } else if let Some(IdRequestRejection::UnsupportedVersion) = err.find() {
        let code = StatusCode::BAD_REQUEST;
        let message = "Unsupported version.";
        Ok(warp::reply::with_status(message, code))
    } else if let Some(IdRequestRejection::InvalidProofs) = err.find() {
        let code = StatusCode::BAD_REQUEST;
        let message = "Invalid proofs.";
        Ok(warp::reply::with_status(message, code))
    } else if let Some(IdRequestRejection::IdVerifierFailure) = err.find() {
        let code = StatusCode::BAD_REQUEST;
        let message = "ID verifier rejected..";
        Ok(warp::reply::with_status(message, code))
    } else if let Some(IdRequestRejection::InternalError) = err.find() {
        let code = StatusCode::INTERNAL_SERVER_ERROR;
        let message = "Internal server error";
        Ok(warp::reply::with_status(message, code))
    } else if let Some(IdRequestRejection::ReuseOfRegId) = err.find() {
        let code = StatusCode::BAD_REQUEST;
        let message = "Reuse of RegId";
        Ok(warp::reply::with_status(message, code))
    } else {
        let code = StatusCode::INTERNAL_SERVER_ERROR;
        let message = "Internal error.";
        Ok(warp::reply::with_status(message, code))
    }
}

/// Asks the identity verifier to verify the person and return the associated
/// attribute list. The attribute list is used to create the identity object
/// that is then signed and saved. If successful a re-direct to the URL where
/// the identity object is available is returned.
async fn create_signed_identity_object(
    server_config: Arc<ServerConfig>,
    db: DB,
    client: Client,
    identity_object_input: ValidatedRequest,
) -> Result<impl Reply, Rejection> {
    let request = identity_object_input.request;

    // Identity verification process between the identity provider and the identity
    // verifier. In this example the identity verifier is queried and will
    // always just return a static attribute list without doing any actual
    // verification of an identity.
    let attribute_list = match client
        .post(server_config.id_verification_url.clone())
        .send()
        .await
    {
        Ok(attribute_list) => match attribute_list.json().await {
            Ok(attribute_list) => attribute_list,
            Err(e) => {
                error!("Could not deserialize response from the verifier {}.", e);
                return Err(warp::reject::custom(IdRequestRejection::IdVerifierFailure));
            }
        },
        Err(e) => {
            error!(
                "Could not retrieve attribute list from the verifier: {}.",
                e
            );
            return Err(warp::reject::custom(IdRequestRejection::InternalError));
        }
    };

    // At this point the identity has been verified, and the identity provider
    // constructs the identity object and signs it. An anonymity revocation
    // record and the identity object are persisted, so that they can be
    // retrieved when needed. The constructed response contains a redirect to a
    // webservice that returns the identity object constructed here.

    // This is hardcoded for the proof-of-concept.
    // Expiry is a year from now.
    let now = YearMonth::now();
    let valid_to_next_year = YearMonth {
        year:  now.year + 1,
        month: now.month,
    };

    let alist = ExampleAttributeList {
        valid_to:     valid_to_next_year,
        created_at:   now,
        alist:        attribute_list,
        max_accounts: 200,
        _phantom:     Default::default(),
    };

    let signature = match sign_identity_object(
        &request,
        &server_config.ip_data.public_ip_info,
        &alist,
        &server_config.ip_data.ip_secret_key,
    ) {
        Ok(signature) => signature,
        Err(e) => {
            error!("Could not sign the identity object {}.", e);
            return Err(warp::reject::custom(IdRequestRejection::InternalError));
        }
    };

    let base16_encoded_id_cred_pub = base16_encode_string(&request.pub_info_for_ip.id_cred_pub);

    ok_or_500!(
        save_revocation_record(&db, &request, &alist),
        "Could not write the revocation record to database."
    );

    let id = IdentityObject {
        pre_identity_object: request,
        alist,
        signature,
    };

    let versioned_id = Versioned::new(VERSION_0, id);

    // Store the created IdentityObject.
    // This is stored so it can later be retrieved by querying via the idCredPub.
    ok_or_500!(
        db.write_identity_object(&base16_encoded_id_cred_pub, &versioned_id),
        "Could not write to database."
    );

    // As a last step we submit the initial account creation to the chain.
    // TODO: We should check beforehand that the regid is fresh and that
    // no account with this regid already exists, since that will lead to failure of
    // account creation.
    let initial_cdi = create_initial_cdi(
        &server_config.ip_data.public_ip_info,
        versioned_id
            .value
            .pre_identity_object
            .pub_info_for_ip
            .clone(),
        &versioned_id.value.alist,
        &server_config.ip_data.ip_cdi_secret_key,
    );

    let submission = json!({
        "type": "initial",
        "contents": initial_cdi,
    });

    // The proxy expects a versioned submission, so that is what we construction.
    let versioned_submission = to_value(&Versioned::new(VERSION_0, submission)).unwrap();

    // Submit and wait for the submission ID.
    match submit_account_creation(
        &client,
        server_config.submit_credential_url.clone(),
        &versioned_submission,
    )
    .await
    {
        Ok(status) => {
            ok_or_500!(
                db.write_pending(&base16_encoded_id_cred_pub, status, versioned_submission),
                "Could not write submission status."
            );
            let query_url_base = server_config.submit_credential_url.clone();
            tokio::spawn(followup(
                client,
                db,
                server_config.submit_credential_url.clone(),
                query_url_base,
                base16_encoded_id_cred_pub.clone(),
            ))
        }
        Err(_) => return Err(warp::reject::custom(IdRequestRejection::ReuseOfRegId)),
    };
    // If we reached here it means we at least have a pending request. We respond
    // with a URL where they will be able to retrieve the ID object.

    // The callback_location has to point to the location where the wallet can
    // retrieve the identity object when it is available.
    let mut retrieve_url = server_config.retrieve_url.clone();
    retrieve_url.set_path(&format!("api/identity/{}", base16_encoded_id_cred_pub));
    let callback_location =
        identity_object_input.redirect_uri.clone() + "#code_uri=" + retrieve_url.as_str();

    info!("Identity was successfully created. Returning URI where it can be retrieved.");

    Ok(warp::reply::with_status(
        warp::reply::with_header(warp::reply(), LOCATION, callback_location),
        StatusCode::FOUND,
    ))
}

/// Validate that the received request is well-formed.
/// This check that all the cryptographic values are valid, and that the zero
/// knowledge proofs in the request are valid.
///
/// The return value is either
///
/// - Ok(ValidatedRequest) if the request is valid or
/// - Err(msg) where `msg` is a string describing the error.
fn extract_and_validate_request(
    server_config: Arc<ServerConfig>,
) -> impl Filter<Extract = (ValidatedRequest,), Error = Rejection> + Clone {
    warp::query().and_then(move |input: Input| {
        let server_config = server_config.clone();
        async move {
            info!("Queried for creating an identity");
            let request: IdentityObjectRequest = from_str(&input.state)
                .map_err(|_| warp::reject::custom(IdRequestRejection::CouldNotParse))?;
            if request.id_object_request.version != VERSION_0 {
                return Err(warp::reject::custom(IdRequestRejection::UnsupportedVersion));
            }
            let request = request.id_object_request.value;

            let context = IPContext {
                ip_info:        &server_config.ip_data.public_ip_info,
                ars_infos:      &server_config.ars.anonymity_revokers,
                global_context: &server_config.global,
            };

            match ip_validate_request(&request, context) {
                Ok(()) => {
                    info!("Request is valid.");
                    Ok(ValidatedRequest {
                        request,
                        redirect_uri: input.redirect_uri,
                    })
                }
                Err(e) => {
                    warn!("Request is invalid {}.", e);
                    Err(warp::reject::custom(IdRequestRejection::InvalidProofs))
                }
            }
        }
    })
}

/// Creates and saves the revocation record to the file system (which should be
/// a database, but for the proof-of-concept we use the file system).
fn save_revocation_record<A: Attribute<id::constants::BaseField>>(
    db: &DB,
    pre_identity_object: &PreIdentityObject<IpPairing, ArCurve>,
    alist: &AttributeList<id::constants::BaseField, A>,
) -> anyhow::Result<()> {
    let ar_record = AnonymityRevocationRecord {
        id_cred_pub:  pre_identity_object.pub_info_for_ip.id_cred_pub,
        ar_data:      pre_identity_object.ip_ar_data.clone(),
        max_accounts: alist.max_accounts,
    };
    let base16_id_cred_pub = base16_encode_string(&ar_record.id_cred_pub);
    db.write_revocation_record(&base16_id_cred_pub, &ar_record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_successful_validation_and_response() {
        // Given
        let request = include_str!("../data/valid_request.json");
        let ip_data_contents = include_str!("../data/identity_provider.json");
        let ar_info_contents = include_str!("../data/anonymity_revokers.json");
        let global_context_contents = include_str!("../data/global.json");

        let ip_data: IpData<IpPairing> = from_str(&ip_data_contents)
            .expect("File did not contain a valid IpData object as JSON.");
        let ar_info: Versioned<ArInfos<ArCurve>> = from_str(&ar_info_contents)
            .expect("File did not contain a valid ArInfos object as JSON");
        assert_eq!(ar_info.version, VERSION_0, "Unsupported ArInfo version.");
        let ars = ar_info.value;
        let global_context: Versioned<GlobalContext<ArCurve>> = from_str(&global_context_contents)
            .expect("File did not contain a valid GlobalContext object as JSON");
        assert_eq!(global_context.version, VERSION_0);
        let global = global_context.value;

        let server_config = Arc::new(ServerConfig {
            ip_data,
            global,
            ars,
            id_verification_url: url::Url::parse("http://localhost/verify").unwrap(),
            retrieve_url: url::Url::parse("http://localhost/retrieve").unwrap(),
            submit_credential_url: url::Url::parse("http://localhost/submitCredential").unwrap(),
        });

        let input = Input {
            state:        request.to_string(),
            redirect_uri: "test".to_string(),
        };

        // When
        let response = extract_and_validate_request(server_config.clone(), input);
        // Then
        assert!(response.is_ok());
    }

    #[test]
    fn test_verify_failed_validation() {
        // Given
        let request = include_str!("../data/fail_validation_request.json");
        let ip_data_contents = include_str!("../data/identity_provider.json");
        let ar_info_contents = include_str!("../data/anonymity_revokers.json");
        let global_context_contents = include_str!("../data/global.json");

        let ip_data: IpData<IpPairing> = from_str(&ip_data_contents)
            .expect("File did not contain a valid IpData object as JSON.");
        let ar_info: Versioned<ArInfos<ArCurve>> = from_str(&ar_info_contents)
            .expect("File did not contain a valid ArInfos object as JSON");
        assert_eq!(ar_info.version, VERSION_0, "Unsupported ArInfo version.");
        let ars = ar_info.value;
        let global_context: Versioned<GlobalContext<ArCurve>> = from_str(&global_context_contents)
            .expect("File did not contain a valid GlobalContext object as JSON");
        assert_eq!(global_context.version, VERSION_0);
        let global = global_context.value;

        let server_config = Arc::new(ServerConfig {
            ip_data,
            global,
            ars,
            id_verification_url: url::Url::parse("http://localhost/verify").unwrap(),
            retrieve_url: url::Url::parse("http://localhost/retrieve").unwrap(),
            submit_credential_url: url::Url::parse("http://localhost/submitCredential").unwrap(),
        });

        let input = Input {
            state:        request.to_string(),
            redirect_uri: "test".to_string(),
        };

        // When
        let response = extract_and_validate_request(server_config, input);

        // Then (the zero knowledge proofs could not be verified, so we fail)
        assert!(response.is_err());
    }
}
