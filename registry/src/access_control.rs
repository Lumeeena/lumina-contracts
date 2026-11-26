use soroban_sdk::{Address, Env, Vec};

use crate::{DataKey, RegistryError};

/// Authenticate a caller without inspecting contract state. Keeping this
/// separate lets entrypoints preserve their existing authentication-before-
/// lookup ordering.
pub(crate) fn require_auth(caller: &Address) {
    caller.require_auth();
}

/// Enforce the standard owner-only rule after the entrypoint has loaded its
/// registration.
pub(crate) fn assert_owner(caller: &Address, owner: &Address) -> Result<(), RegistryError> {
    if caller == owner {
        Ok(())
    } else {
        Err(RegistryError::NotOwner)
    }
}

/// Enforce an owner check while retaining an entrypoint-specific error code.
pub(crate) fn assert_owner_with_error(
    caller: &Address,
    owner: &Address,
    error: RegistryError,
) -> Result<(), RegistryError> {
    if caller == owner {
        Ok(())
    } else {
        Err(error)
    }
}

/// Enforce the owner-or-admin rule used by ownership transfer. The legacy
/// single-admin slot remains a compatibility authorization for this path only.
pub(crate) fn assert_owner_or_admin(
    env: &Env,
    caller: &Address,
    owner: &Address,
) -> Result<(), RegistryError> {
    if caller == owner {
        return Ok(());
    }

    let admins = admin_index(env);
    let is_admin = admins.contains(caller);
    let is_legacy_admin = env
        .storage()
        .instance()
        .get::<DataKey, Address>(&DataKey::Admin)
        .map(|admin| admin == *caller)
        .unwrap_or(false);

    if is_admin || is_legacy_admin {
        return Ok(());
    }

    if !env.storage().instance().has(&DataKey::Admins)
        && !env.storage().instance().has(&DataKey::Admin)
    {
        return Err(RegistryError::NotInitialized);
    }

    Err(RegistryError::Unauthorized)
}

fn admin_index(env: &Env) -> Vec<Address> {
    env.storage()
        .instance()
        .get(&DataKey::Admins)
        .unwrap_or(Vec::new(env))
}

pub(crate) fn assert_admin(admins: &Vec<Address>, caller: &Address) -> Result<(), RegistryError> {
    if admins.is_empty() {
        return Err(RegistryError::NotInitialized);
    }
    if !admins.contains(caller) {
        return Err(RegistryError::NotAdmin);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn owner_rule_accepts_owner_and_rejects_other_callers() {
        let env = Env::default();
        let owner = Address::generate(&env);
        let other = Address::generate(&env);

        assert_eq!(assert_owner(&owner, &owner), Ok(()));
        assert_eq!(assert_owner(&other, &owner), Err(RegistryError::NotOwner));
    }

    #[test]
    fn owner_rule_can_preserve_entrypoint_specific_error() {
        let env = Env::default();
        let owner = Address::generate(&env);
        let other = Address::generate(&env);

        assert_eq!(
            assert_owner_with_error(&other, &owner, RegistryError::Unauthorized),
            Err(RegistryError::Unauthorized)
        );
    }

    #[test]
    fn admin_rule_distinguishes_uninitialized_non_admin_and_admin() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let other = Address::generate(&env);
        let empty = Vec::new(&env);

        assert_eq!(
            assert_admin(&empty, &admin),
            Err(RegistryError::NotInitialized)
        );

        let mut admins = Vec::new(&env);
        admins.push_back(admin.clone());
        assert_eq!(assert_admin(&admins, &other), Err(RegistryError::NotAdmin));
        assert_eq!(assert_admin(&admins, &admin), Ok(()));
    }
}
