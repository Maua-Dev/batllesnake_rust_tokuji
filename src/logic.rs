// Bem-vindo ao
// __________         __    __  .__                               __
// \______   \_____ _/  |__/  |_|  |   ____   ______ ____ _____  |  | __ ____
//  |    |  _/\__  \   __\   __\  | _/ __ \ /  ___//    \__  \ |  |/ // __ \
//  |    |   \ / __ \|  |  |  | |  |_\  ___/ \___ \|   |  \/ __ \|    <\  ___/
//  |________/(______/__|  |__| |____/\_____>______>___|__(______/__|__\_____>
//
// ESTE É O ARQUIVO QUE VOCÊ VAI EDITAR. Todo o resto do projeto existe
// só para levar o estado do jogo até as quatro funções abaixo.
//
// Estratégia (pensada para partidas com 4+ cobras): descarta o que mata na
// hora, evita becos (flood fill) e head-to-head com cobras maiores, e só busca
// comida quando está com fome ou não é a maior da mesa.
// Documentação: https://docs.battlesnake.com

use crate::models::{Battlesnake, Coord, GameState};
use serde_json::{json, Value};
use std::collections::VecDeque;
use tracing::info;

/// GET / — chamado quando você cadastra a cobra no site e a cada partida.
/// Controla a aparência dela. Opções de cabeça, cauda e cor:
/// https://docs.battlesnake.com/guides/customizations
pub fn info() -> Value {
    info!("INFO");

    json!({
        "apiversion": "1",
        "author": "Tokuji",
        "color": "#0077B6", // ciano escuro / azul petróleo
        "head": "sand-worm",
        "tail": "round-bum",
        "version": "1.0.0"
    })
}

/// POST /start — chamado uma vez, quando a partida começa.
/// Bom lugar para preparar qualquer estado inicial.
pub fn start(state: &GameState) {
    info!("JOGO COMEÇOU (partida {})", state.game.id);
}

/// POST /end — chamado uma vez, quando a partida termina.
pub fn end(state: &GameState) {
    info!("FIM DE JOGO após {} turnos", state.turn);
}

const MOVES: [(&str, i32, i32); 4] = [
    ("up", 0, 1),
    ("down", 0, -1),
    ("left", -1, 0),
    ("right", 1, 0),
];

// Pesos da pontuação. Ordem de gravidade: beco > head-to-head > o resto.
const TRAP: i32 = -1000; // espaço alcançável menor que o nosso corpo
const H2H_BIGGER: i32 = -500; // casa que um adversário maior alcança junto com a gente
const H2H_EQUAL: i32 = -300; // empate de tamanho: morrem os dois
const H2H_SMALLER: i32 = 10; // adversário menor: o head-to-head é nosso
const HAZARD: i32 = -20;
const HUNGER_MARGIN: i32 = 15; // vida que queremos sobrando ao chegar na comida

/// Casas ocupadas do tabuleiro neste turno.
struct Grid {
    w: i32,
    h: i32,
    blocked: Vec<bool>,
}

impl Grid {
    fn new(state: &GameState) -> Grid {
        let (w, h) = (state.board.width, state.board.height);
        let mut grid = Grid {
            w,
            h,
            blocked: vec![false; (w * h) as usize],
        };
        for snake in &state.board.snakes {
            // A cauda sai do lugar neste turno, a não ser que a cobra tenha
            // acabado de comer (aí os dois últimos segmentos ficam empilhados).
            let n = snake.body.len();
            let tail_moves = n >= 2 && snake.body[n - 1] != snake.body[n - 2];
            let solid = if tail_moves {
                &snake.body[..n - 1]
            } else {
                &snake.body[..]
            };
            for &c in solid {
                if grid.inside(c) {
                    let i = grid.idx(c);
                    grid.blocked[i] = true;
                }
            }
        }
        grid
    }

    fn inside(&self, c: Coord) -> bool {
        c.x >= 0 && c.y >= 0 && c.x < self.w && c.y < self.h
    }

    fn idx(&self, c: Coord) -> usize {
        (c.y * self.w + c.x) as usize
    }

    fn free(&self, c: Coord) -> bool {
        self.inside(c) && !self.blocked[self.idx(c)]
    }

    /// BFS a partir de `start`: chama `visit(casa, distância)` para cada casa
    /// livre alcançável; parar quando `visit` devolver true.
    // ponytail: os corpos ficam parados no BFS; casas que liberam com o tempo
    // (caudas andando) não contam. Pessimista em espaço apertado.
    fn bfs(&self, start: Coord, mut visit: impl FnMut(Coord, i32) -> bool) {
        let mut seen = vec![false; self.blocked.len()];
        let mut queue = VecDeque::from([(start, 0)]);
        seen[self.idx(start)] = true;
        while let Some((c, d)) = queue.pop_front() {
            if visit(c, d) {
                return;
            }
            for (_, dx, dy) in MOVES {
                let next = Coord {
                    x: c.x + dx,
                    y: c.y + dy,
                };
                if self.free(next) && !seen[self.idx(next)] {
                    seen[self.idx(next)] = true;
                    queue.push_back((next, d + 1));
                }
            }
        }
    }
}

fn distance(a: Coord, b: Coord) -> i32 {
    (a.x - b.x).abs() + (a.y - b.y).abs()
}

/// POST /move — chamado a cada turno. Aqui mora a inteligência da sua cobra.
/// Precisa devolver "up", "down", "left" ou "right".
/// Exemplo do JSON recebido: https://docs.battlesnake.com/api/example-move
///
/// Cada direção que não mata na hora ganha uma nota; vence a maior.
pub fn get_move(state: &GameState) -> Value {
    let me = &state.you;
    let head = me.body[0];
    let grid = Grid::new(state);
    let opponents: Vec<&Battlesnake> = state
        .board
        .snakes
        .iter()
        .filter(|s| s.id != me.id)
        .collect();
    let biggest_opponent = opponents.iter().map(|s| s.length).max().unwrap_or(0);
    let hazard_damage = state
        .game
        .ruleset
        .get("settings")
        .and_then(|s| s.get("hazardDamagePerTurn"))
        .and_then(Value::as_i64)
        .unwrap_or(14) as i32;

    let mut best: Option<(&str, i32)> = None;
    for (name, dx, dy) in MOVES {
        let target = Coord {
            x: head.x + dx,
            y: head.y + dy,
        };
        if !grid.free(target) {
            continue; // parede, pescoço ou corpo: morte certa
        }
        let eats = state.board.food.contains(&target);
        let in_hazard = state.board.hazards.contains(&target);
        if in_hazard && !eats && me.health <= hazard_damage + 1 {
            continue; // o hazard zera a vida
        }

        let mut score = 0;

        for o in &opponents {
            if distance(o.head, target) == 1 {
                score += match o.length.cmp(&me.length) {
                    std::cmp::Ordering::Greater => H2H_BIGGER,
                    std::cmp::Ordering::Equal => H2H_EQUAL,
                    std::cmp::Ordering::Less => H2H_SMALLER,
                };
            }
        }

        let mut space = 0;
        grid.bfs(target, |_, _| {
            space += 1;
            false
        });
        if space < me.length {
            score += TRAP;
        }
        // Espaço acima de 2x o corpo não faz diferença: aí quem decide é a comida.
        score += space.min(2 * me.length);

        if in_hazard {
            score += HAZARD;
        }

        // Comida mais próxima que nenhum adversário maior/igual alcança antes.
        let mut food_dist = None;
        grid.bfs(target, |c, d| {
            let found = state.board.food.contains(&c)
                && !opponents
                    .iter()
                    .any(|o| o.length >= me.length && distance(o.head, c) <= d + 1);
            if found {
                food_dist = Some(d);
            }
            found
        });
        if let Some(d) = food_dist {
            if me.health - 1 - d < HUNGER_MARGIN {
                score += (200 - 5 * d).max(0); // fome: comida vira prioridade
            } else if me.length <= biggest_opponent {
                score += (40 - 2 * d).max(0); // crescer para ganhar os head-to-heads
            }
        }

        if best.is_none_or(|(_, s)| score > s) {
            best = Some((name, score));
        }
    }

    let Some((chosen, score)) = best else {
        info!("MOVE {}: sem saída!", state.turn);
        return json!({ "move": "up" });
    };
    info!("MOVE {}: {} (nota {})", state.turn, chosen, score);
    json!({ "move": chosen })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Battlesnake, Board, Coord, Game};
    use std::collections::HashMap;

    fn c(x: i32, y: i32) -> Coord {
        Coord { x, y }
    }

    fn snake(id: &str, body: Vec<Coord>) -> Battlesnake {
        Battlesnake {
            id: id.to_string(),
            name: id.to_string(),
            health: 100,
            head: body[0],
            length: body.len() as i32,
            body,
            latency: None,
            shout: None,
        }
    }

    /// Estado com a gente (`me`) e os adversários, sem comida.
    fn arena(me: Battlesnake, opponents: Vec<Battlesnake>) -> GameState {
        let mut state = game_state(me.body[0], me.body[1]);
        state.board.food = vec![];
        state.board.snakes = std::iter::once(me.clone()).chain(opponents).collect();
        state.you = me;
        state
    }

    #[test]
    fn evita_head_to_head_com_cobra_maior() {
        // Adversário maior com a cabeça em (7,5): a casa (6,5) é disputada.
        let me = snake("eu", vec![c(5, 5), c(4, 5), c(3, 5)]);
        let big = snake(
            "grande",
            vec![c(7, 5), c(8, 5), c(9, 5), c(10, 5), c(10, 4)],
        );
        assert_ne!(chosen_move(&arena(me, vec![big])), "right");
    }

    #[test]
    fn evita_beco() {
        // Esquerda leva a um bolsão de 5 casas, fechado pelo corpo do
        // adversário (que acabou de comer, então a cauda não sai): cabem
        // menos casas que o nosso corpo (6). Subir é a única saída boa.
        let me = snake(
            "eu",
            vec![c(3, 0), c(4, 0), c(5, 0), c(6, 0), c(7, 0), c(8, 0)],
        );
        let wall = snake(
            "parede",
            vec![
                c(0, 4),
                c(0, 3),
                c(0, 2),
                c(1, 2),
                c(2, 2),
                c(2, 1),
                c(2, 1),
            ],
        );
        assert_eq!(chosen_move(&arena(me, vec![wall])), "up");
    }

    #[test]
    fn cauda_conta_como_casa_livre() {
        // Enrolada no canto: a única saída é a casa onde está a própria cauda.
        let me = snake("eu", vec![c(0, 0), c(0, 1), c(1, 1), c(1, 0)]);
        assert_eq!(chosen_move(&arena(me, vec![])), "right");
    }

    #[test]
    fn com_fome_vai_para_a_comida() {
        let mut me = snake("eu", vec![c(5, 5), c(5, 4), c(5, 3)]);
        me.health = 10;
        let mut state = arena(me, vec![]);
        state.board.food = vec![c(7, 5)];
        assert_eq!(chosen_move(&state), "right");
    }

    /// Monta um estado de jogo mínimo para os testes, com a cobra deitada
    /// na horizontal: cabeça em `head` e pescoço em `neck`.
    fn game_state(head: Coord, neck: Coord) -> GameState {
        let you = Battlesnake {
            id: "minha-cobra".to_string(),
            name: "MinhaCobra".to_string(),
            health: 100,
            body: vec![
                head,
                neck,
                Coord {
                    x: neck.x,
                    y: neck.y - 1,
                },
            ],
            head,
            length: 3,
            latency: Some("50".to_string()),
            shout: None,
        };

        GameState {
            game: Game {
                id: "partida-de-teste".to_string(),
                ruleset: HashMap::new(),
                map: Some("standard".to_string()),
                timeout: 500,
            },
            turn: 4,
            board: Board {
                height: 11,
                width: 11,
                food: vec![Coord { x: 5, y: 5 }],
                hazards: vec![],
                snakes: vec![you.clone()],
            },
            you,
        }
    }

    fn chosen_move(state: &GameState) -> String {
        get_move(state)["move"].as_str().unwrap().to_string()
    }

    #[test]
    fn info_devolve_os_campos_obrigatorios() {
        let response = info();

        assert_eq!(response["apiversion"], "1");
        assert!(response.get("author").is_some());
        assert!(response.get("color").is_some());
        assert!(response.get("head").is_some());
        assert!(response.get("tail").is_some());
    }

    #[test]
    fn move_devolve_sempre_uma_direcao_valida() {
        let state = game_state(Coord { x: 5, y: 4 }, Coord { x: 4, y: 4 });

        for _ in 0..50 {
            let direction = chosen_move(&state);
            assert!(
                ["up", "down", "left", "right"].contains(&direction.as_str()),
                "direção inválida: {direction}"
            );
        }
    }

    #[test]
    fn nunca_volta_por_cima_do_pescoco() {
        // pescoço à esquerda da cabeça: "left" seria andar para trás
        let state = game_state(Coord { x: 5, y: 4 }, Coord { x: 4, y: 4 });
        for _ in 0..50 {
            assert_ne!(chosen_move(&state), "left");
        }

        // pescoço à direita da cabeça: "right" seria andar para trás
        let state = game_state(Coord { x: 5, y: 4 }, Coord { x: 6, y: 4 });
        for _ in 0..50 {
            assert_ne!(chosen_move(&state), "right");
        }

        // pescoço abaixo da cabeça: "down" seria andar para trás
        let state = game_state(Coord { x: 5, y: 4 }, Coord { x: 5, y: 3 });
        for _ in 0..50 {
            assert_ne!(chosen_move(&state), "down");
        }

        // pescoço acima da cabeça: "up" seria andar para trás
        let state = game_state(Coord { x: 5, y: 4 }, Coord { x: 5, y: 5 });
        for _ in 0..50 {
            assert_ne!(chosen_move(&state), "up");
        }
    }

    #[test]
    fn evita_parede_quando_tem_opcao() {
        // Cobra no canto inferior esquerdo, pescoço à direita da cabeça:
        // não pode ir para right (pescoço) nem left (x=-1) nem down (y=-1).
        // A única opção segura é "up".
        let state = game_state(Coord { x: 0, y: 0 }, Coord { x: 1, y: 0 });
        for _ in 0..50 {
            let direction = chosen_move(&state);
            assert!(
                ["up", "down", "left", "right"].contains(&direction.as_str()),
                "direção inválida: {direction}"
            );
            assert_ne!(direction, "left", "foi para fora do tabuleiro (esquerda)");
            assert_ne!(direction, "down", "foi para fora do tabuleiro (baixo)");
        }
    }

    #[test]
    fn evita_proprio_corpo_quando_tem_opcao() {
        // Cabeça em (5,4), pescoço à esquerda (4,4), corpo acima em (5,5).
        // Restam right e down. Verificamos que nunca escolhe "left" nem "up".
        let head = Coord { x: 5, y: 4 };
        let neck = Coord { x: 4, y: 4 };
        let mut state = game_state(head, neck);
        state.you.body = vec![head, neck, Coord { x: 5, y: 5 }, Coord { x: 4, y: 3 }];
        state.board.snakes = vec![state.you.clone()];

        for _ in 0..50 {
            let direction = chosen_move(&state);
            assert_ne!(direction, "left", "voltou pelo pescoço");
            assert_ne!(direction, "up", "bateu no próprio corpo");
            assert!(["right", "down"].contains(&direction.as_str()));
        }
    }

    #[test]
    fn comportamento_definido_sem_safe_moves() {
        // Cabeça no canto (0,0), pescoço acima (0,1) — bloqueia up.
        // left (x=-1) e down (y=-1) saem do tabuleiro.
        // Somente right estaria livre, mas o helper adiciona um segmento
        // em (1,0) para fechar todas as saídas e testar o fallback.
        //
        // Independentemente de qual direção for escolhida, não pode lançar
        // pânico e deve ser uma das quatro direções válidas.
        let head = Coord { x: 0, y: 0 };
        let neck = Coord { x: 0, y: 1 };
        let mut state = game_state(head, neck);
        // Adiciona um segmento do corpo à direita para bloquear "right"
        state.you.body.push(Coord { x: 1, y: 0 });
        state.board.snakes = vec![state.you.clone()];

        let direction = chosen_move(&state);
        assert!(
            ["up", "down", "left", "right"].contains(&direction.as_str()),
            "fallback retornou direção inválida: {direction}"
        );
    }
}
