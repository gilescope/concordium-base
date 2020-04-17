use clap::{App, AppSettings, Arg, SubCommand};

use client_server_helpers::*;
use curve_arithmetic::{Curve, Pairing};
use dodis_yampolskiy_prf::secret as prf;
use ed25519_dalek as ed25519;
use id::{account_holder::*, ffi::*, identity_provider::*, secret_sharing::Threshold, types::*};
use pairing::bls12_381::{Bls12, G1};
use std::collections::btree_map::BTreeMap;

use rand::{rngs::ThreadRng, *};

use pedersen_scheme::Value as PedersenValue;

use crypto_common::base16_encode_string;
use serde_json::json;
use std::path::Path;

use ec_vrf_ed25519 as vrf;

use aggregate_sig as agg;

use either::Either::Left;

type ExampleCurve = G1;

type ExampleAttribute = AttributeKind;

type ExampleAttributeList = AttributeList<<Bls12 as Pairing>::ScalarField, ExampleAttribute>;

fn main() {
    let app =
        App::new("Generate bakers with accounts for inclusion in genesis or just beta accounts.")
            .version("0.31830988618")
            .author("Concordium")
            .setting(AppSettings::ArgRequiredElseHelp)
            .global_setting(AppSettings::ColoredHelp)
            .arg(
                Arg::with_name("ip-data")
                    .long("ip-data")
                    .value_name("FILE")
                    .help(
                        "File with all information about the identity provider that is going to \
                         sign all the credentials.",
                    )
                    .required(false)
                    .global(true),
            )
            .arg(
                Arg::with_name("global")
                    .long("global")
                    .value_name("FILE")
                    .help("File with global parameters.")
                    .default_value(GLOBAL_CONTEXT)
                    .required(false)
                    .global(true),
            )
            .subcommand(
                SubCommand::with_name("create-bakers")
                    .about("Create new bakers.")
                    .arg(
                        Arg::with_name("num")
                            .long("num")
                            .value_name("N")
                            .help("Number of bakers to generate.")
                            .required(true),
                    )
                    .arg(
                        Arg::with_name("num_finalizers")
                            .long("num_finalizers")
                            .value_name("F")
                            .help("The amount of finalizers to generate. Defaults to all bakers.")
                            .required(false),
                    )
                    .arg(
                        Arg::with_name("balance")
                            .long("balance")
                            .value_name("AMOUNT")
                            .help("Balance on each of the baker accounts.")
                            .default_value("35000000000"),
                    ),
            )
            .subcommand(
                SubCommand::with_name("create-accounts")
                    .about("Create a given number of accounts. These do not delegate to any baker.")
                    .arg(
                        Arg::with_name("num")
                            .long("num")
                            .value_name("N")
                            .help("Number of accounts to generate.")
                            .required(true),
                    )
                    .arg(
                        Arg::with_name("template")
                            .long("template")
                            .value_name("TEMPLATE")
                            .help(
                                "Template on how to name accounts; they will be name \
                                 TEMPLATE-$N.json.",
                            )
                            .default_value("account"),
                    ),
            );

    let matches = app.get_matches();

    let mut csprng = thread_rng();

    // Load identity provider and anonymity revokers.
    let ip_data_path = Path::new(matches.value_of("ip-data").unwrap());
    let (ip_info, ip_secret_key) =
        match read_json_from_file::<_, IpData<Bls12, ExampleCurve>>(&ip_data_path) {
            Ok(IpData {
                public_ip_info,
                ip_secret_key,
                ..
            }) => (public_ip_info, ip_secret_key),
            Err(e) => {
                eprintln!("Could not parse identity issuer JSON because: {}", e);
                return;
            }
        };

    let context = make_context_from_ip_info(ip_info.clone(), ChoiceArParameters {
        // use all anonymity revokers.
        ar_identities: ip_info.ip_ars.ars.iter().map(|ar| ar.ar_identity).collect(),
        // all but one threshold
        threshold: Threshold((ip_info.ip_ars.ars.len() - 1) as _),
    })
    .expect("Constructed AR data is valid.");

    // we also read the global context from another json file (called
    // global.context). We need commitment keys and other data in there.
    let global_ctx = {
        if let Some(gc) = read_global_context(
            matches
                .value_of("global")
                .expect("We have a default value, so should exist."),
        ) {
            gc
        } else {
            eprintln!("Cannot read global context information database. Terminating.");
            return;
        }
    };

    // Roughly one year
    let generate_account = |csprng: &mut ThreadRng| {
        let secret = ExampleCurve::generate_scalar(csprng);
        let ah_info = CredentialHolderInfo::<ExampleCurve> {
            id_cred: IdCredentials {
                id_cred_sec: PedersenValue::new(secret),
            },
        };

        // Choose prf key.
        let prf_key = prf::SecretKey::generate(csprng);

        // Expire in 1 year from now.
        let created_at = YearMonth::now();
        let valid_to = {
            let mut now = YearMonth::now();
            now.year += 1;
            now
        };

        // no attributes
        let alist = BTreeMap::new();
        let aci = AccCredentialInfo {
            cred_holder_info: ah_info,
            prf_key,
        };

        let attributes = ExampleAttributeList {
            valid_to,
            created_at,
            max_accounts: 238,
            alist,
            _phantom: Default::default(),
        };

        let (pio, randomness) = generate_pio(&context, &aci);

        let sig_ok = verify_credentials(&pio, &ip_info, &attributes, &ip_secret_key);

        let ip_sig = sig_ok.expect("There is an error in signing");

        let policy = Policy {
            valid_to,
            created_at,
            policy_vec: BTreeMap::new(),
            _phantom: Default::default(),
        };

        let mut keys = BTreeMap::new();
        keys.insert(KeyIndex(0), ed25519::Keypair::generate(csprng));
        keys.insert(KeyIndex(1), ed25519::Keypair::generate(csprng));
        keys.insert(KeyIndex(2), ed25519::Keypair::generate(csprng));

        let acc_data = AccountData {
            keys,
            existing: Left(SignatureThreshold(2)),
        };

        let id_object = IdentityObject {
            pre_identity_object: pio,
            alist:               attributes,
            signature:           ip_sig,
        };

        let id_object_use_data = IdObjectUseData { aci, randomness };

        let cdi = generate_cdi(
            &ip_info,
            &global_ctx,
            &id_object,
            &id_object_use_data,
            53,
            &policy,
            &acc_data,
        )
        .expect("We should have constructed valid data.");

        let address = AccountAddress::new(&cdi.values.reg_id);

        let acc_keys = AccountKeys {
            keys:      acc_data
                .keys
                .iter()
                .map(|(&idx, kp)| (idx, VerifyKey::from(kp)))
                .collect(),
            threshold: SignatureThreshold(2),
        };

        // output private account data
        let account_data_json = json!({
            "address": address,
            "accountData": acc_data,
            "credential": cdi,
            "aci": id_object_use_data.aci,
        });
        (account_data_json, cdi, acc_keys, address)
    };

    if let Some(matches) = matches.subcommand_matches("create-bakers") {
        let num_bakers = match matches.value_of("num").unwrap().parse() {
            Ok(n) => n,
            Err(err) => {
                eprintln!("Could not parse the number of bakers: {}", err);
                return;
            }
        };

        let num_finalizers = match matches.value_of("num_finalizers") {
            None => num_bakers,
            Some(arg) => match arg.parse() {
                Ok(n) => n,
                Err(err) => {
                    eprintln!("Could not parse the number of finalizers: {}", err);
                    return;
                }
            },
        };

        let balance = match matches.value_of("balance").unwrap().parse::<u64>() {
            Ok(n) => n,
            Err(err) => {
                eprintln!("Could not parse balance: {}", err);
                return;
            }
        };

        let mut bakers = Vec::with_capacity(num_bakers);
        for baker in 0..num_bakers {
            let (account_data_json, credential_json, account_keys, address_json) =
                generate_account(&mut csprng);
            if let Err(err) =
                write_json_to_file(&format!("baker-{}-account.json", baker), &account_data_json)
            {
                eprintln!(
                    "Could not output account data for baker {}, because {}.",
                    baker, err
                );
            }

            // vrf keypair
            let vrf_key = vrf::Keypair::generate(&mut csprng);
            // signature keypair
            let sign_key = ed25519::Keypair::generate(&mut csprng);

            let agg_sign_key = agg::SecretKey::<Bls12>::generate(&mut csprng);
            let agg_verify_key = agg::PublicKey::from_secret(agg_sign_key);

            // Output baker vrf and election keys in a json file.
            let baker_data_json = json!({
                "electionPrivateKey": base16_encode_string(&vrf_key.secret),
                "electionVerifyKey": base16_encode_string(&vrf_key.public),
                "signatureSignKey": base16_encode_string(&sign_key.secret),
                "signatureVerifyKey": base16_encode_string(&sign_key.public),
                "aggregationSignKey": base16_encode_string(&agg_sign_key),
                "aggregationVerifyKey": base16_encode_string(&agg_verify_key),
            });

            if let Err(err) = write_json_to_file(
                &format!("baker-{}-credentials.json", baker),
                &baker_data_json,
            ) {
                eprintln!(
                    "Could not output baker credential for baker {}, because {}.",
                    baker, err
                );
            }

            // Finally store a json value storing public data for this baker.
            let public_baker_data = json!({
                "electionVerifyKey": base16_encode_string(&vrf_key.public),
                "signatureVerifyKey": base16_encode_string(&sign_key.public),
                "aggregationVerifyKey": base16_encode_string(&agg_verify_key),
                "finalizer": baker < num_finalizers,
                "account": json!({
                    "address": address_json,
                    "accountKeys": account_keys,
                    "balance": balance,
                    "credential": credential_json
                })
            });
            bakers.push(public_baker_data);
        }

        // finally output all of the bakers in one file. This is used to generate
        // genesis.
        if let Err(err) = write_json_to_file("bakers.json", &json!(bakers)) {
            eprintln!("Could not output bakers.json file because {}.", err)
        }
    }

    if let Some(matches) = matches.subcommand_matches("create-accounts") {
        let num_accounts = match matches.value_of("num").unwrap().parse() {
            Ok(n) => n,
            Err(err) => {
                eprintln!("Could not parse the number of bakers: {}", err);
                return;
            }
        };

        let prefix = matches.value_of("template").unwrap(); // has default value, will be present.

        let mut accounts = Vec::with_capacity(num_accounts);
        for acc_num in 0..num_accounts {
            let (account_data_json, credential_json, account_keys, address_json) =
                generate_account(&mut csprng);
            let public_account_data = json!({
                "schemeId": "Ed25519",
                "accountKeys": account_keys,
                "address": address_json,
                "balance": 1_000_000_000_000u64,
                "credential": credential_json
            });
            accounts.push(public_account_data);

            if let Err(err) = write_json_to_file(
                &format!("{}-{}.json", prefix, acc_num),
                &json!(account_data_json),
            ) {
                eprintln!(
                    "Could not output beta-account-{}.json file because {}.",
                    acc_num, err
                )
            }
        }
        // finally output all of the public account data in one file. This is used to
        // generate genesis.
        if let Err(err) = write_json_to_file(&format!("{}s.json", prefix), &json!(accounts)) {
            eprintln!("Could not output beta-accounts.json file because {}.", err)
        }
    }
}
