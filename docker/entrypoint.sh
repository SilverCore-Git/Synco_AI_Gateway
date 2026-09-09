#!/usr/bin/env bash
set -u

# Démarre Ollama en arrière-plan (l'image de base ollama/ollama fournit son propre binaire).
ollama serve &
OLLAMA_PID=$!

echo "[entrypoint] En attente d'Ollama sur 127.0.0.1:11434..."
until (exec 3<>/dev/tcp/127.0.0.1/11434) 2>/dev/null; do
    sleep 0.5
done
exec 3>&- 2>/dev/null || true
echo "[entrypoint] Ollama prêt."

# Télécharge le modèle par défaut s'il n'est pas déjà présent — idempotent, "ollama pull"
# vérifie le manifeste local avant de retélécharger quoi que ce soit. Peut prendre plusieurs
# minutes au premier démarrage selon la taille du modèle et la bande passante.
if [ -n "${OLLAMA_MODEL:-}" ]; then
    echo "[entrypoint] Vérification/téléchargement du modèle '$OLLAMA_MODEL'..."
    ollama pull "$OLLAMA_MODEL" || echo "[entrypoint] Échec du téléchargement de '$OLLAMA_MODEL' — on continue quand même, réessayez avec 'docker exec <container> ollama pull $OLLAMA_MODEL'."
fi

# Lance la passerelle en arrière-plan également, pour pouvoir attendre sur les deux processus.
/usr/local/bin/synco_ai_gateway &
GATEWAY_PID=$!
echo "[entrypoint] Passerelle démarrée (pid $GATEWAY_PID), Ollama (pid $OLLAMA_PID)."

# Propage un arrêt (docker stop) aux deux processus plutôt que de les laisser en zombies.
trap 'echo "[entrypoint] Arrêt..."; kill -TERM "$OLLAMA_PID" "$GATEWAY_PID" 2>/dev/null' TERM INT

# Si l'un des deux meurt, on arrête tout le conteneur (laisse la politique de restart Docker
# décider de relancer) plutôt que de continuer à moitié fonctionnel.
wait -n "$OLLAMA_PID" "$GATEWAY_PID"
EXIT_CODE=$?
echo "[entrypoint] Un processus s'est arrêté (code $EXIT_CODE), extinction du conteneur."
kill -TERM "$OLLAMA_PID" "$GATEWAY_PID" 2>/dev/null
exit "$EXIT_CODE"
