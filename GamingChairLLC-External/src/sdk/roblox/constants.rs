//! Humanoid state ids, matching the game's `Enum.HumanoidStateType`.

pub const HUMANOID_STATES: &[(i32, &str)] = &[
    (0, "FallingDown"),
    (1, "Ragdoll"),
    (2, "GettingUp"),
    (3, "Jumping"),
    (4, "Swimming"),
    (5, "Freefall"),
    (6, "Flying"),
    (7, "Landed"),
    (8, "Running"),
    (10, "RunningNoPhysics"),
    (11, "StrafingNoPhysics"),
    (12, "Climbing"),
    (13, "Seated"),
    (14, "PlatformStanding"),
    (15, "Dead"),
    (16, "Physics"),
    (18, "None"),
];

pub fn state_name(id: i32) -> String {
    HUMANOID_STATES
        .iter()
        .find(|(state_id, _)| *state_id == id)
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_else(|| format!("Unknown({id})"))
}

pub fn state_id(name: &str) -> Option<i32> {
    HUMANOID_STATES
        .iter()
        .find(|(_, state_name)| state_name.eq_ignore_ascii_case(name))
        .map(|(id, _)| *id)
}
