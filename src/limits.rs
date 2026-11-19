//! Limites de concurrence du relais : globale et par utilisateur (`sub` du ticket). Une place est
//! prise avant d'appeler Ollama et libérée par `Drop` : fin du flux, erreur, ou déconnexion du client
//! (la requête, ou le flux, est alors abandonné et le jeton rendu).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Debug, PartialEq, Eq)]
pub enum LimitError {
    /// Trop de requêtes simultanées pour cet utilisateur.
    User,
    /// Passerelle saturée.
    Global,
}

impl LimitError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::User => "too_many_requests_user",
            Self::Global => "too_many_requests",
        }
    }
}

pub struct Limiter {
    global: Arc<Semaphore>,
    per_user: Mutex<HashMap<String, usize>>,
    max_per_user: usize,
}

/// Place occupée ; rendue au `Drop`.
pub struct Slot {
    _global: OwnedSemaphorePermit,
    limiter: Arc<Limiter>,
    sub: String,
}

impl Limiter {
    pub fn new(max_global: usize, max_per_user: usize) -> Arc<Self> {
        Arc::new(Self {
            global: Arc::new(Semaphore::new(max_global)),
            per_user: Mutex::new(HashMap::new()),
            max_per_user,
        })
    }

    pub fn try_acquire(self: &Arc<Self>, sub: &str) -> Result<Slot, LimitError> {
        {
            let mut users = self.per_user.lock().unwrap_or_else(|e| e.into_inner());
            let n = users.entry(sub.to_string()).or_insert(0);
            if *n >= self.max_per_user {
                return Err(LimitError::User);
            }
            *n += 1;
        }
        match self.global.clone().try_acquire_owned() {
            Ok(permit) => Ok(Slot {
                _global: permit,
                limiter: self.clone(),
                sub: sub.to_string(),
            }),
            Err(_) => {
                self.release_user(sub);
                Err(LimitError::Global)
            }
        }
    }

    fn release_user(&self, sub: &str) {
        let mut users = self.per_user.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = users.get_mut(sub) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                users.remove(sub);
            }
        }
    }

    #[cfg(test)]
    fn tracked_users(&self) -> usize {
        self.per_user.lock().map(|u| u.len()).unwrap_or(0)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.limiter.release_user(&self.sub);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limite_par_utilisateur_puis_globale() {
        let l = Limiter::new(3, 2);
        let a1 = l.try_acquire("a").unwrap();
        let _a2 = l.try_acquire("a").unwrap();
        assert_eq!(l.try_acquire("a").err(), Some(LimitError::User));
        let _b1 = l.try_acquire("b").unwrap();
        assert_eq!(l.try_acquire("c").err(), Some(LimitError::Global));
        drop(a1);
        assert!(l.try_acquire("c").is_ok());
    }

    #[test]
    fn la_liberation_rend_les_deux_compteurs_et_nettoie_la_table() {
        let l = Limiter::new(1, 1);
        let s = l.try_acquire("a").unwrap();
        assert_eq!(l.try_acquire("b").err(), Some(LimitError::Global));
        assert_eq!(
            l.tracked_users(),
            1,
            "un refus global ne laisse pas de compteur utilisateur"
        );
        drop(s);
        assert_eq!(l.tracked_users(), 0);
        assert!(l.try_acquire("a").is_ok());
    }
}
