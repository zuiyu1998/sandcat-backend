use std::process::Command;

pub trait BuilderExt {
    fn with_sqlx_type(self, path: &[&str]) -> Self;
    fn with_derive_builder(self, path: &[&str]) -> Self;
    fn with_derive_builder_into(self, path: &str, attr: &[&str]) -> Self;
    fn with_derive_builder_option(self, path: &str, attr: &[&str]) -> Self;
    fn with_serde(self, path: &[&str]) -> Self;
}

impl BuilderExt for tonic_build::Builder {
    // set sqlx::Type for ReservationStatus
    fn with_sqlx_type(self, path: &[&str]) -> Self {
        // fold func: do somethin with given closure to given initial value; return final value
        path.iter().fold(self, |acc, path| {
            acc.type_attribute(path, "#[derive(sqlx::Type)]")
        })
    }

    fn with_derive_builder(self, path: &[&str]) -> Self {
        path.iter().fold(self, |acc, path| {
            acc.type_attribute(path, "#[derive(derive_builder::Builder)]")
        })
    }

    fn with_derive_builder_into(self, path: &str, field: &[&str]) -> Self {
        field.iter().fold(self, |acc, field| {
            acc.field_attribute(
                format!("{path}.{field}"),
                "#[builder(setter(into), default)]",
            )
        })
    }

    fn with_derive_builder_option(self, path: &str, field: &[&str]) -> Self {
        field.iter().fold(self, |acc, field| {
            acc.field_attribute(
                format!("{path}.{field}"),
                "#[builder(setter(strip_option, into), default)]",
            )
        })
    }

    fn with_serde(self, path: &[&str]) -> Self {
        path.iter().fold(self, |acc, path| {
            acc.type_attribute(path, "#[derive(serde::Serialize, serde::Deserialize)]")
        })
    }
}
fn main() {
    tonic_build::configure()
        .out_dir("src/pb")
        .field_attribute("User.password", "#[serde(skip_serializing)]")
        .field_attribute("User.salt", "#[serde(skip_serializing)]")
        .with_serde(&[
            "PlatformType",
            "Msg",
            "MsgContent",
            "Mention",
            "MsgRead",
            "MsgToDb",
            "GetDbMsgRequest",
            "GetDbMessagesRequest",
            "DelMsgRequest",
            "UserAndGroupID",
            "User",
            "UserUpdate",
            "UserWithMatchType",
            "Friend",
            "FriendInfo",
            "Friendship",
            "FriendshipWithUser",
            "FsCreate",
            "FsCreateRequest",
            "FsUpdate",
            "UpdateRemarkRequest",
            "DeleteFriendRequest",
            "AgreeReply",
            "Single",
            "MsgResponse",
            "GroupInfo",
            "GroupInviteNew",
            "RemoveMemberRequest",
            "GetMemberReq",
            "GroupMember",
            "GroupMemberRole",
            "GroupCreate",
            "GroupUpdate",
            "GroupInvitation",
            "GetGroupAndMembersResp",
            "SingleCallInvite",
            "SingleCallInviteAnswer",
            "SingleCallInviteNotAnswer",
            "SingleCallInviteCancel",
            "SingleCallOffer",
            "Hangup",
            "AgreeSingleCall",
            "Candidate",
        ])
        .with_sqlx_type(&["FriendshipStatus", "GroupMemberRole"])
        .compile(&["protos/messages.proto"], &["protos"])
        .unwrap();

    // execute cargo fmt command
    Command::new("cargo").arg("fmt").output().unwrap();

    println!("cargo: rerun-if-changed=abi/protos/messages.proto");
}
