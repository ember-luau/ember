use crate::commands::index::index_url_for;
use crate::error::Error;
use crate::net::auth;
use crate::net::registry;
use crate::project::manifest::Manifest;
use crate::registry::index::{Index, Refresh};
use crate::ui;
use clap::{Args, Subcommand};

#[derive(Args, Debug)]
pub struct AuthArgs {
    /** The index whose login to use: ember, a key under [indices], or a
    URL. Without it, the login is ember's own */
    #[arg(short, long, global = true)]
    pub index: Option<String>,

    #[command(subcommand)]
    pub command: AuthCommand,
}

#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    /// Log in through emberpm.dev in the browser, or store a token you already have
    Login {
        /// An Ember token (ember_...) or a GitHub token to store, instead of the browser login
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

/** pesde's auth commands. ember keeps one login, an Ember token of the
account its registry maps to scope owners. without `--index` they work on
that login whatever the default index is, so a wally default never blocks
a login. `--index` only checks that the named index takes logins at all. */
pub fn run(args: AuthArgs) -> Result<(), Error> {
    match args.command {
        AuthCommand::Login { token } => {
            login_index(args.index.as_deref())?;
            match token {
                Some(token) => {
                    let token = token.trim();
                    /* the API checks the token now, so a bad one fails here
                    and not at publish. a GitHub token is traded for an
                    Ember token */
                    let api = registry::api_url();
                    let credentials = if token.starts_with(auth::TOKEN_PREFIX) {
                        auth::check(&api, token)?
                    } else {
                        auth::exchange(&api, token)?
                    };
                    auth::save(&credentials)?;
                    ui::print_success(&format!("Logged in as {}", credentials.login));
                }
                None => {
                    auth::login()?;
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

/** checks that the index that `--index` names takes a login: its config
has an OAuth client id. wally and pesde indices answer that they take
none. ember's own index takes one, so without `--index` nothing is cloned. */
fn login_index(index: Option<&str>) -> Result<(), Error> {
    let Some(name) = index else {
        return Ok(());
    };
    let url = index_url_for(Manifest::load().ok().as_ref(), name)?;
    let index = Index::open(&url, Refresh::Ttl)?;
    match index.github_oauth_id() {
        Some(_) => Ok(()),
        None => Err(Error::PublishNotSupported(url)),
    }
}
