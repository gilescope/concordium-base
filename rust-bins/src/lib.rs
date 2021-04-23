use crypto_common::*;
use curve_arithmetic::*;
use id::{ffi::*, types::*};
use pairing::bls12_381::Bls12;
use serde::{de::DeserializeOwned, Serialize as SerdeSerialize};
use serde_json::{to_string_pretty, to_writer_pretty};
use std::{
    fmt::Debug,
    fs::File,
    io::{self, BufReader},
    path::Path,
    str::FromStr,
};

pub type ExampleCurve = <Bls12 as Pairing>::G1;

pub type ExampleAttribute = AttributeKind;

pub type ExampleAttributeList = AttributeList<<Bls12 as Pairing>::ScalarField, ExampleAttribute>;

pub static GLOBAL_CONTEXT: &str = "database/global.json";
pub static IDENTITY_PROVIDERS: &str = "database/identity_providers.json";

/// Read an object containing a versioned global context from the given file.
/// Currently only version 0 is supported.
pub fn read_global_context<P: AsRef<Path> + Debug>(
    filename: P,
) -> Option<GlobalContext<ExampleCurve>> {
    let params: Versioned<serde_json::Value> = read_json_from_file(filename).ok()?;
    match params.version {
        Version { value: 0 } => serde_json::from_value(params.value).ok(),
        _ => None,
    }
}

/// Read ip-info, deciding on how to parse based on the version.
pub fn read_ip_info<P: AsRef<Path> + Debug>(filename: P) -> io::Result<IpInfo<Bls12>> {
    let params: Versioned<serde_json::Value> = read_json_from_file(filename)?;
    match params.version {
        Version { value: 0 } => Ok(serde_json::from_value(params.value)?),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid identity provider version {}.", other),
        )),
    }
}

/// Read id_object, deciding on how to parse based on the version.
pub fn read_id_object<P: AsRef<Path> + Debug>(
    filename: P,
) -> io::Result<IdentityObject<Bls12, ExampleCurve, ExampleAttribute>> {
    let params: Versioned<serde_json::Value> = read_json_from_file(filename)?;
    match params.version {
        Version { value: 0 } => Ok(serde_json::from_value(params.value)?),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid identity object version {}.", other),
        )),
    }
}

/// Read id_use_data, deciding on how to parse based on the version.
pub fn read_id_use_data<P: AsRef<Path> + Debug>(
    filename: P,
) -> io::Result<IdObjectUseData<Bls12, ExampleCurve>> {
    let params: Versioned<serde_json::Value> = read_json_from_file(filename)?;
    match params.version {
        Version { value: 0 } => Ok(serde_json::from_value(params.value)?),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid identity object use data object version {}.", other),
        )),
    }
}

/// Read pre-identity object, deciding on how to parse based on the version.
pub fn read_pre_identity_object<P: AsRef<Path> + Debug>(
    filename: P,
) -> io::Result<PreIdentityObject<Bls12, ExampleCurve>> {
    let params: Versioned<serde_json::Value> = read_json_from_file(filename)?;
    match params.version {
        Version { value: 0 } => Ok(serde_json::from_value(params.value)?),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid identity object use data object version {}.", other),
        )),
    }
}

/// Read identity providers versioned with a single version at the top-level.
/// All values are parsed according to that version.
pub fn read_identity_providers<P: AsRef<Path> + Debug>(filename: P) -> io::Result<IpInfos<Bls12>> {
    let vips: Versioned<serde_json::Value> = read_json_from_file(filename)?;
    match vips.version {
        Version { value: 0 } => Ok(serde_json::from_value(vips.value)?),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid identity providers version {}.", other),
        )),
    }
}

/// Read a single identity provider versioned with a single version at the
/// top-level. All values are parsed according to that version.
pub fn read_identity_provider<P: AsRef<Path> + Debug>(filename: P) -> io::Result<IpInfo<Bls12>> {
    let vips: Versioned<serde_json::Value> = read_json_from_file(filename)?;
    match vips.version {
        Version { value: 0 } => Ok(serde_json::from_value(vips.value)?),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid identity provider version {}.", other),
        )),
    }
}

/// Read anonymity revokers from a file, determining how to parse them from the
/// version number.
pub fn read_anonymity_revokers<P: AsRef<Path> + Debug>(
    filename: P,
) -> io::Result<ArInfos<ExampleCurve>> {
    let vars: Versioned<serde_json::Value> = read_json_from_file(filename)?;
    match vars.version {
        Version { value: 0 } => Ok(serde_json::from_value(vars.value)?),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid anonymity revokers version {}.", other),
        )),
    }
}

/// Read anonymity revokers from a file, determining how to parse them from the
/// version number.
pub fn read_credential<P: AsRef<Path> + Debug>(
    filename: P,
) -> io::Result<CredentialDeploymentInfo<Bls12, ExampleCurve, ExampleAttribute>> {
    let vars: Versioned<serde_json::Value> = read_json_from_file(filename)?;
    match vars.version {
        Version { value: 0 } => Ok(serde_json::from_value(vars.value)?),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid credential version {}.", other),
        )),
    }
}

/// Parse YYYYMM as YearMonth
pub fn parse_yearmonth(input: &str) -> Option<YearMonth> { YearMonth::from_str(input).ok() }

/// Output json to a file, pretty printed.
pub fn write_json_to_file<P: AsRef<Path>, T: SerdeSerialize>(filepath: P, v: &T) -> io::Result<()> {
    let file = File::create(filepath)?;
    Ok(to_writer_pretty(file, v)?)
}

/// Output json to standard output, pretty printed.
pub fn output_json<T: SerdeSerialize>(v: &T) {
    println!("{}", to_string_pretty(v).unwrap());
}

pub fn read_json_from_file<P, T>(path: P) -> io::Result<T>
where
    P: AsRef<Path> + Debug,
    T: DeserializeOwned, {
    let file = File::open(path)?;

    let reader = BufReader::new(file);
    let u = serde_json::from_reader(reader)?;
    Ok(u)
}

/// Ask for a password and a confirmation
/// It doesn't ask for a confirmation if `skip_if_empty` is `true` and the
/// password is empty
pub fn ask_for_password_confirm(
    prompt: &str,
    skip_if_empty: bool,
) -> Result<String, std::io::Error> {
    loop {
        let pass = rpassword::read_password_from_tty(Some(prompt))?;
        if !(skip_if_empty && pass.is_empty()) {
            let pass2 = rpassword::read_password_from_tty(Some("Re-enter password: "))?;
            if pass != pass2 {
                println!("Passwords were not equal. Try again.");
                continue;
            }
        }
        return Ok(pass);
    }
}
