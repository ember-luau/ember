use crate::commands::index::index_url_for;
use crate::error::Error;
use crate::net::auth::{self, Credentials};
use crate::net::github::GithubAPI;
use crate::project::manifest::Manifest;
use crate::registry::index::{Index, Refresh};
use crate::ui;
use clap::{Args, Subcommand};

#[derive(Args, Debug)]
pub struct AuthArgs {
    /** The index whose login to use: ember, a key under [indices], or a
    URL. Without it, the login is ember's own GitHub login */
    #[arg(short, long, global = true)]
    pub index: Option<String>,

    #[command(subcommand)]
    pub command: AuthCommand,
}

#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    /// Log in with GitHub, or store a token you already have
    Login {
        /// The token to store, instead of the GitHub device flow
        #[arg(short, long)]
        token: Option<String>,
    },
    /// Remove the stored login
    Logout,
    /// Print the GitHub user the stored login belongs to
    Whoami,
    /// Print the stored token
    Token,
}

/** pesde's auth commands. ember keeps one GitHub login, the one its
registry maps to scope owners. without `--index` they work on that login
whatever the default index is, so a wally default never blocks a login.
`--index` only checks that the named index takes logins at all. */
pub fn run(args: AuthArgs) -> Result<(), Error> {
    match args.command {
        AuthCommand::Login { token } => {
            let client_id = login_index(args.index.as_deref())?;
            match token {
                Some(token) => {
                    let token = token.trim().to_string();
                    // the login comes from GitHub, so a bad token fails here and not at publish
                    let user = GithubAPI::with_token(&token).get_user()?;
                    auth::save(&Credentials {
                        token,
                        login: user.login.clone(),
                    })?;
                    ui::print_success(&format!("Logged in as {}", user.login));
                }
                None => {
                    auth::login(&client_id)?;
                }
            }
        }
        AuthCommand::Logout => {
            login_index(args.index.as_deref())?;
            match auth::load()? {
                Some(credentials) => {
                    auth::clear()?;
                    ui::print_success(&format!("Logged out {}", credentials.login));
                }
                None => println!("No login is stored"),
            }
        }
        AuthCommand::Whoami => {
            login_index(args.index.as_deref())?;
            let credentials = auth::load()?.ok_or(Error::NotLoggedIn)?;
            println!("{}", credentials.login);
        }
        AuthCommand::Token => {
            login_index(args.index.as_deref())?;
            let credentials = auth::load()?.ok_or(Error::NotLoggedIn)?;
            println!("{}", credentials.token);
        }
    }
    Ok(())
}

/** the index's OAuth client id, which is also the proof that it takes a
login. ember's own index unless `--index` names another; wally and pesde
indices answer that they take none. */
fn login_index(index: Option<&str>) -> Result<String, Error> {
    let url = match index {
        Some(name) => index_url_for(Manifest::load().ok().as_ref(), name)?,
        None => crate::project::manifest::DEFAULT_INDEX_URL.to_string(),
    };
    let index = Index::open(&url, Refresh::Ttl)?;
    index
        .github_oauth_id()
        .map(str::to_string)
        .ok_or(Error::PublishNotSupported(url))
}
