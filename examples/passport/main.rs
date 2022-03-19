mod ark_sha256;
mod issuance_checker;
mod params;
mod passport_dump;
mod passport_info;
mod preds;
mod sig_verif;

use crate::{
    issuance_checker::{IssuanceReq, PassportHashChecker},
    params::{
        ComForest, ComTree, ComTreePath, ForestProvingKey, ForestVerifyingKey, PassportComScheme,
        PassportComSchemeG, PredProof, PredProvingKey, PredVerifyingKey, TreeProvingKey,
        TreeVerifyingKey, H, HG, MERKLE_CRH_PARAM, SIG_HASH_LEN, STATE_ID_LEN,
    },
    passport_dump::PassportDump,
    passport_info::{PersonalInfo, PersonalInfoVar},
    preds::AgeAndFaceChecker,
    sig_verif::{load_usa_pubkey, IssuerPubkey},
};

use zeronym::{
    attrs::Attrs,
    link::{
        link_proofs, verif_link_proof, GsCrs, LinkProofCtx, LinkVerifyingKey, PredPublicInputs,
    },
    pred::{prove_birth, prove_pred, verify_birth, verify_pred},
    Com,
};

use std::fs::File;

use ark_bls12_381::{Bls12_381, Fr};
use ark_ff::UniformRand;
use ark_std::rand::Rng;

const TREE_HEIGHT: u32 = 32;
const FOREST_SIZE: usize = 10;

// Sample parameters for passport validation. All passports must expire some time after TODAY, and
// be issued by ISSUING_STATE
const TODAY: u32 = 20220101u32;
const MAX_VALID_YEARS: u32 = 10u32;
const TWENTY_ONE_YEARS_AGO: u32 = TODAY - 210000;
const ISSUING_STATE: [u8; STATE_ID_LEN] = *b"USA";

fn load_dump() -> PassportDump {
    let file = File::open("examples/passport/full_dump.json").unwrap();
    serde_json::from_reader(file).unwrap()
}

fn rand_tree<R: Rng>(rng: &mut R) -> ComTree {
    let mut tree = ComTree::empty(MERKLE_CRH_PARAM.clone(), TREE_HEIGHT);
    let idx: u16 = rng.gen();
    let leaf = Com::<PassportComScheme>::rand(rng);
    tree.insert(idx as u64, &leaf);
    tree
}

fn rand_forest<R: Rng>(rng: &mut R) -> ComForest {
    let trees = (0..FOREST_SIZE).map(|_| rand_tree(rng)).collect();
    ComForest { trees }
}

struct IssuerState {
    /// The forest of commitments
    com_forest: ComForest,
    /// The next free tree to insert a commitment
    next_free_tree: usize,
    /// The next free leaf in that tree to insert a commitment
    next_free_leaf: u64,
}

fn gen_issuance_crs<R: Rng>(rng: &mut R) -> (PredProvingKey, PredVerifyingKey) {
    // Generate the hash checker circuit's CRS
    let pk = zeronym::pred::gen_pred_crs::<
        _,
        _,
        Bls12_381,
        PersonalInfo,
        PersonalInfoVar,
        PassportComScheme,
        PassportComSchemeG,
        H,
        HG,
    >(rng, PassportHashChecker::default())
    .unwrap();

    (pk.clone(), pk.prepare_verifying_key())
}

fn gen_ageface_crs<R: Rng>(rng: &mut R) -> (PredProvingKey, PredVerifyingKey) {
    // Generate the hash checker circuit's CRS
    let pk = zeronym::pred::gen_pred_crs::<
        _,
        _,
        Bls12_381,
        PersonalInfo,
        PersonalInfoVar,
        PassportComScheme,
        PassportComSchemeG,
        H,
        HG,
    >(rng, AgeAndFaceChecker::default())
    .unwrap();

    (pk.clone(), pk.prepare_verifying_key())
}

fn gen_tree_crs<R: Rng>(rng: &mut R) -> (TreeProvingKey, TreeVerifyingKey) {
    // Generate the predicate circuit's CRS
    let pk = zeronym::com_tree::gen_tree_memb_crs::<
        _,
        Bls12_381,
        PersonalInfo,
        PassportComScheme,
        PassportComSchemeG,
        H,
        HG,
    >(rng, MERKLE_CRH_PARAM.clone(), TREE_HEIGHT)
    .unwrap();

    (pk.clone(), pk.prepare_verifying_key())
}

fn gen_forest_crs<R: Rng>(rng: &mut R) -> (ForestProvingKey, ForestVerifyingKey) {
    // Generate the predicate circuit's CRS
    let pk = zeronym::com_forest::gen_forest_memb_crs::<
        _,
        Bls12_381,
        PersonalInfo,
        PassportComScheme,
        PassportComSchemeG,
        H,
        HG,
    >(rng, FOREST_SIZE)
    .unwrap();

    (pk.clone(), pk.prepare_verifying_key())
}

/// Makes a random new issuer state
fn init_issuer<R: Rng>(rng: &mut R) -> IssuerState {
    let com_forest = rand_forest(rng);
    let next_free_tree = rng.gen_range(0..FOREST_SIZE);
    let next_free_leaf = rng.gen_range(0..2u64.pow(TREE_HEIGHT - 1));

    IssuerState {
        com_forest,
        next_free_tree,
        next_free_leaf,
    }
}

/// An issuer takes an issuance request and validates it
fn issue(state: &mut IssuerState, birth_vk: &PredVerifyingKey, req: &IssuanceReq) -> ComTreePath {
    // Check that the hash was computed correctly
    let hash_checker =
        PassportHashChecker::from_issuance_req(req, ISSUING_STATE, TODAY, MAX_VALID_YEARS);
    assert!(verify_birth(birth_vk, &req.hash_proof, &hash_checker, &req.attrs_com).unwrap());

    // Now check that the signature of the hash is correct
    let sig_pubkey = load_usa_pubkey();
    assert!(sig_pubkey.verify(&req.sig, &req.econtent_hash));

    // Insert
    state.com_forest.trees[state.next_free_tree].insert(state.next_free_leaf, &req.attrs_com)
}

/// With their passport, a user constructs a `PersonalInfo` struct and requests issuance
fn user_req_issuance<R: Rng>(
    rng: &mut R,
    issuance_pk: &PredProvingKey,
) -> (PersonalInfo, IssuanceReq) {
    // Load the passport and parse it into a `PersonalInfo` struct
    let dump = load_dump();
    let my_info = PersonalInfo::from_passport(rng, &dump, TODAY);
    let attrs_com = my_info.commit();

    // Make a hash checker struct using our private data
    let hash_checker =
        PassportHashChecker::from_passport(&dump, ISSUING_STATE, TODAY, MAX_VALID_YEARS);

    // Prove the passport hash is correctly computed
    let hash_proof = prove_birth(rng, issuance_pk, hash_checker, my_info.clone()).unwrap();

    // Now put together the issuance request
    let req = IssuanceReq {
        attrs_com,
        econtent_hash: dump.econtent_hash(),
        sig: dump.sig,
        hash_proof,
    };

    (my_info, req)
}

/// Returns an instance of an `AgeFaceChecker`. Public parameters are date and the authenticating
/// user's biometric hash
fn get_ageface_checker(info: &PersonalInfo) -> AgeAndFaceChecker {
    AgeAndFaceChecker {
        threshold_dob: Fr::from(TWENTY_ONE_YEARS_AGO),
        face_hash: info.biometrics_hash(),
    }
}

/// User constructs a predicate proof for their age and face
fn user_prove_ageface<R: Rng>(
    rng: &mut R,
    ageface_pk: &PredProvingKey,
    ageface_checker: &AgeAndFaceChecker,
    info: &PersonalInfo,
    auth_path: &ComTreePath,
) -> PredProof {
    // Compute the proof wrt the public parameters
    let proof = prove_pred(
        rng,
        ageface_pk,
        ageface_checker.clone(),
        info.clone(),
        auth_path,
    )
    .unwrap();

    // DEBUG: Assert that the proof verifies
    assert!(zeronym::pred::verify_pred(
        &ageface_pk.prepare_verifying_key(),
        &proof,
        ageface_checker,
        &info.commit(),
        &auth_path.root(),
    )
    .unwrap());

    proof
}

fn main() {
    let mut rng = ark_std::test_rng();

    // Generate all the Groth16 and Groth-Sahai proving and verifying keys
    let (issuance_pk, issuance_vk) = gen_issuance_crs(&mut rng);
    let (ageface_pk, ageface_vk) = gen_ageface_crs(&mut rng);
    let (tree_pk, tree_vk) = gen_tree_crs(&mut rng);
    let (forest_pk, forest_vk) = gen_forest_crs(&mut rng);
    let gs_crs = GsCrs::rand(&mut rng);
    println!("GLOBAL: Generated CRSs");

    // Generate a random initial state for the issuer
    let mut issuer_state = init_issuer(&mut rng);

    // The user dumps their passport and makes an issuance request
    let (personal_info, issuance_req) = user_req_issuance(&mut rng, &issuance_pk);
    let cred = personal_info.commit();
    println!("USER: Requested issuance");

    // The issuer validates the passport and issues the credential
    let auth_path = issue(&mut issuer_state, &issuance_vk, &issuance_req);
    println!("ISSUER: Issuance request granted");

    //
    // A user walks into a bar...
    //
    println!("USER");

    // User wants to prove age and face. They precompute this proof
    let ageface_checker = get_ageface_checker(&personal_info);
    let ageface_proof = user_prove_ageface(
        &mut rng,
        &ageface_pk,
        &ageface_checker,
        &personal_info,
        &auth_path,
    );
    println!("\tComputed age+face proof");
    // User gets all the roots from the issuer
    let roots = issuer_state.com_forest.roots();
    // Now user proves tree and forest membership
    let tree_proof = auth_path
        .prove_membership(&mut rng, &tree_pk, &*MERKLE_CRH_PARAM, cred)
        .unwrap();
    let forest_proof = roots
        .prove_membership(&mut rng, &forest_pk, auth_path.root(), cred)
        .unwrap();
    println!("\tComputed tree and forest memebership proofs");
    // User prepares the predicate public inputs
    let mut pred_inputs = PredPublicInputs::default();
    pred_inputs.prepare_pred_checker(&ageface_vk, &ageface_checker);

    // Now the user links everything
    let link_vk = LinkVerifyingKey {
        gs_crs,
        pred_inputs,
        com_forest_roots: roots,
        forest_verif_key: forest_vk,
        tree_verif_key: tree_vk,
        pred_verif_keys: vec![ageface_vk.clone()],
    };
    let link_ctx = LinkProofCtx {
        attrs_com: cred,
        merkle_root: auth_path.root(),
        forest_proof,
        tree_proof,
        pred_proofs: vec![ageface_proof],
        vk: link_vk.clone(),
    };
    let link_proof = link_proofs(&mut rng, &link_ctx);
    println!("\tLinked proofs");

    //
    // The bouncer takes a look
    //
    println!("BOUNCER");

    // First the bouncer needs to get all the public parameters for their verifying key. Part is
    // fixed and part is given by the user. Specifically, biometrics_hash is supplied by the user,
    // and everything else is fixed. (we just use the vk from above)
    let link_vk = link_vk;
    let biometrics = personal_info.biometrics;
    let ageface_checker = AgeAndFaceChecker {
        threshold_dob: Fr::from(TWENTY_ONE_YEARS_AGO),
        face_hash: biometrics.hash(),
    };
    println!("\tDownloaded user's biometrics");
    // Use the previous link_vk. It's all predetermined values except for the ageface_checker
    // contents
    let mut link_vk = link_vk;
    // User prepares the predicate public inputs
    link_vk.pred_inputs = {
        let mut pred_inputs = PredPublicInputs::default();
        pred_inputs.prepare_pred_checker(&ageface_vk, &ageface_checker);
        pred_inputs
    };
    println!("\tCreated verification key");
    // Bouncer checks the proof
    assert!(verif_link_proof(&link_proof, &link_vk));

    println!("The bouncer unlatches the velvet rope. The user walks through.");
}
