use std::collections::VecDeque;
use zokrates_pest_ast::*;
use crate::front::zsharp::blocks::*;
use std::collections::HashMap;
use std::collections::HashSet;

pub fn optimize_block(
    mut bls: Vec<Block>,
    mut entry_bl: usize
) -> (Vec<Block>, usize) {
    let (successor, predecessor, exit_bls) = construct_flow_graph(&bls, entry_bl);
    println!("\n\n--\nControl Flow Graph:\n");
    println!("Successor:");
    for s in 0..successor.len() {
        print!("Block {}: ", s);
        for b in successor[s].iter() {
            print!("{} ", b);
        }
        println!("\n");
    }
    println!("Predecessor:");
    for p in 0..predecessor.len() {
        print!("Block {}: ", p);
        for b in predecessor[p].iter() {
            print!("{} ", b);
        }
        println!("\n");
    }
    print!("Exit Blocks:");
    for b in exit_bls {
        print!(" {}", b);
    }
    println!("--\n");
    (bls, entry_bl) = dead_block_elimination(bls, entry_bl);
    return (bls, entry_bl);
} 

// If bc is a statement of form %RP = val,
// return val
fn rp_find_val(bc: BlockContent) -> Option<usize> {
    // We can ignore memory for now
    // The only case currently is %RP on the left & constant on the right
    if let BlockContent::Stmt(Statement::Definition(d)) = bc {
        if let TypedIdentifierOrAssignee::Assignee(a) = &d.lhs[0] {
            if a.id.value == "%RP".to_string() {
                if let Expression::Literal(LiteralExpression::DecimalLiteral(dle)) = &d.expression {
                    let tmp_bl: usize = dle.value.value.trim().parse().expect("Dead Block Elimination failed: %RP is assigned to a non-constant value");
                    return Some(tmp_bl);
                } else {
                    panic!("Dead Block Elimination failed: %RP is assigned to a non-constant value")
                }
            }
        }
    }
    return None;
}

// If bc is a statement of form %RP = old_val and old_val is a key in val_map,
// replace it with %RP = val_map[val]
fn rp_replacement_stmt(bc: BlockContent, val_map: HashMap<usize, usize>) -> Option<BlockContent> {
    if let BlockContent::Stmt(Statement::Definition(d)) = bc {
        if let TypedIdentifierOrAssignee::Assignee(a) = &d.lhs[0] {
            if a.id.value == "%RP".to_string() {
                if let Expression::Literal(LiteralExpression::DecimalLiteral(dle)) = &d.expression {
                    let tmp_bl: usize = dle.value.value.trim().parse().unwrap();
                    if let Some(new_bl) = val_map.get(&tmp_bl) {
                        let new_rp_stmt = Statement::Definition(DefinitionStatement {
                            lhs: vec![TypedIdentifierOrAssignee::Assignee(Assignee {
                                id: IdentifierExpression {
                                    value: "%RP".to_string(),
                                    span: Span::new("", 0, 0).unwrap()
                                },
                                accesses: Vec::new(),
                                span: Span::new("", 0, 0).unwrap()
                            })],
                            expression: Expression::Literal(LiteralExpression::DecimalLiteral(DecimalLiteralExpression {
                                value: DecimalNumber {
                                    value: new_bl.to_string(),
                                    span: Span::new("", 0, 0).unwrap()
                                },
                                suffix: Some(DecimalSuffix::Field(FieldSuffix {
                                    span: Span::new("", 0, 0).unwrap()
                                })),
                                span: Span::new("", 0, 0).unwrap()
                            })),
                            span: Span::new("", 0, 0).unwrap()
                        });
                        return Some(BlockContent::Stmt(new_rp_stmt));
                    }
                }
            }
        }
    }
    return None;
}

// Return value: successor, rp_successor, visited, next_bls
fn flow_graph_transition<const IS_RP: bool>(
    cur_bl: &usize,
    next_bl: NextBlock,
    rp_slot: usize,
    mut successor: Vec<HashSet<usize>>,
    mut rp_successor: Vec<HashSet<usize>>,
    mut visited: Vec<bool>,
    mut next_bls: VecDeque<usize>
) -> (Vec<HashSet<usize>>, Vec<HashSet<usize>>, Vec<bool>, VecDeque<usize>) {

    match next_bl {
        NextBlock::Label(tmp_bl) => {
            // Add next_bl to successor of cur_bl if not RP
            if !IS_RP {
                successor[*cur_bl].insert(tmp_bl);
            }
            
            let old_rp_successor = rp_successor[tmp_bl].clone();
            // If rp_slot is not 0, append rp_slot to rp_successor of tmp_bl
            // unless we are dealing with the RP block.
            // If rp_slot is 0 or if we are dealing with the RP block,
            // let next_bl inherit rp_successor of cur_bl
            if rp_slot != 0 && !IS_RP {
                // Function call
                rp_successor[tmp_bl].insert(rp_slot);
            } else {
                // No function call
                for i in rp_successor[*cur_bl].clone().iter() {
                    rp_successor[tmp_bl].insert(*i);
                }     
            }

            // If next_bl is not visited or if rp_successor of tmp__bl changes,
            // append tmp_bl to next_bls
            if !visited[tmp_bl] || rp_successor[tmp_bl] != old_rp_successor {
                let _ = std::mem::replace(&mut visited[tmp_bl], true);
                next_bls.push_back(tmp_bl);
            }
        }
        NextBlock::Rp() => {
            if rp_successor.len() == 0 {
                panic!("Control flow graph construction fails: reaching end of function point but %RP not set!")
            }
            // Add everything in rp_successor of cur_bl to successor of cur_bl
            for i in rp_successor[*cur_bl].iter() {
                successor[*cur_bl].insert(*i);
            }
            // Whatever that rp is should already be in next_bls
        }
    }
    return (successor, rp_successor, visited, next_bls);
}

// Construct a flow graph from a set of blocks
// Return value:
// ret[0]: map from block to all its successors (no need to use HashMap since every block should exists right now)
// ret[1]: map from block to all its predecessors
// ret[2]: list of all blocks that ends with ProgTerm
fn construct_flow_graph(
    bls: &Vec<Block>,
    entry_bl: usize
) -> (Vec<HashSet<usize>>, Vec<HashSet<usize>>, Vec<usize>) {
    let bl_size = bls.len();
    
    // list of all blocks that ends with ProgTerm
    let mut exit_bls: Vec<usize> = Vec::new();
    
    // Start from entry_bl, do a BFS, add all blocks in its terminator to its successor
    // When we reach a function call (i.e., %RP is set), add value of %RP to the callee's rp_successor
    // Propagate rp_successor until we reach an rp() terminator, at that point, append rp_successor to successor
    // We don't care about blocks that won't be touched by BFS, they'll get eliminated anyways
    let mut successor: Vec<HashSet<usize>> = Vec::new();
    let mut rp_successor: Vec<HashSet<usize>> = Vec::new();
    let mut visited: Vec<bool> = Vec::new();
    // predecessor is just the inverse of successor
    let mut predecessor: Vec<HashSet<usize>> = Vec::new();
    for _ in 0..bl_size {
        successor.push(HashSet::new());
        rp_successor.push(HashSet::new());
        visited.push(false);
        predecessor.push(HashSet::new());
    }

    let mut next_bls: VecDeque<usize> = VecDeque::new();
    let _ = std::mem::replace(&mut visited[entry_bl], true);
    next_bls.push_back(entry_bl);
    while !next_bls.is_empty() {
        let cur_bl = next_bls.pop_front().unwrap();

        // If we encounter any %RP = <counter>, record down <counter> to rp_slot
        // By definition, %RP cannot be 0
        // There shouldn't be multiple %RP's, but if there is, we only care about the last one
        let mut rp_slot = 0;
        
        for i in 0..bls[cur_bl].instructions.len() {
            let bc = bls[cur_bl].instructions[i].clone();
            if let Some(tmp_bl) = rp_find_val(bc) {
                rp_slot = tmp_bl;
            }
        }

        // Process RP block
        if rp_slot != 0 {
            (successor, rp_successor, visited, next_bls) = 
                flow_graph_transition::<true>(&cur_bl, NextBlock::Label(rp_slot), rp_slot, successor, rp_successor, visited, next_bls);
        }

        // Append everything in the terminator of cur_bl to next_bls
        // according to flow_graph_transition
        match bls[cur_bl].terminator.clone() {
            BlockTerminator::Transition(t) => {
                (successor, rp_successor, visited, next_bls) = 
                    flow_graph_transition::<false>(&cur_bl, t.tblock, rp_slot, successor, rp_successor, visited, next_bls);
                (successor, rp_successor, visited, next_bls) = 
                    flow_graph_transition::<false>(&cur_bl, t.fblock, rp_slot, successor, rp_successor, visited, next_bls);
            }
            BlockTerminator::Coda(n) => {
                (successor, rp_successor, visited, next_bls) = 
                    flow_graph_transition::<false>(&cur_bl, n, rp_slot, successor, rp_successor, visited, next_bls);
            }
            BlockTerminator::FuncCall(_) => { panic!("Blocks pending optimization should not have FuncCall as terminator.") }
            BlockTerminator::ProgTerm() => { exit_bls.push(cur_bl); }
        }
    }

    for i in 0..bls.len() {
        for j in successor[i].iter() {
            predecessor[*j].insert(i);
        }
    }
    return (successor, predecessor, exit_bls);
}

// Remove all the dead blocks in the list
// Rename all block labels so that they are still consecutive
// Return value: bls, entry_bl, exit_bl
fn dead_block_elimination(
    bls: Vec<Block>,
    entry_bl: usize
) -> (Vec<Block>, usize) {      
    let old_size = bls.len();
    
    // Visited: have we ever visited the block in the following DFS?
    let mut visited: Vec<bool> = Vec::new();
    for _ in 0..old_size {
        visited.push(false);
    }

    // BFS through all blocks starting from entry_bl
    // Use next_bls to record all the nodes that we will visit next
    // Whenever we encounter `%RP = <const>` we need to visit that block as well,
    // however, we don't need to do anything if a block terminates with rp() (i.e. end of function call)
    let mut next_bls: VecDeque<usize> = VecDeque::new();
    let _ = std::mem::replace(&mut visited[entry_bl], true);
    next_bls.push_back(entry_bl);
    while !next_bls.is_empty() {
        let cur_bl = next_bls.pop_front().unwrap();
        
        // If we encounter any %RP = <counter>, append <counter> to next_bls
        for i in 0..bls[cur_bl].instructions.len() {
            let bc = bls[cur_bl].instructions[i].clone();
            if let Some(tmp_bl) = rp_find_val(bc) {
                if !visited[tmp_bl] {
                    let _ = std::mem::replace(&mut visited[tmp_bl], true);
                    next_bls.push_back(tmp_bl);
                }
            }
        }
        
        // Append everything in the terminator of cur_bl to next_bls
        // if they have not been visited before
        match bls[cur_bl].terminator.clone() {
            BlockTerminator::Transition(t) => {
                if let NextBlock::Label(tmp_bl) = t.tblock {
                    if !visited[tmp_bl] {
                        let _ = std::mem::replace(&mut visited[tmp_bl], true);
                        next_bls.push_back(tmp_bl);
                    }
                }
                if let NextBlock::Label(tmp_bl) = t.fblock {
                    if !visited[tmp_bl] {
                        let _ = std::mem::replace(&mut visited[tmp_bl], true);
                        next_bls.push_back(tmp_bl);
                    }
                }
            }
            BlockTerminator::Coda(n) => {
                if let NextBlock::Label(tmp_bl) = n {
                    if !visited[tmp_bl] {
                        let _ = std::mem::replace(&mut visited[tmp_bl], true);
                        next_bls.push_back(tmp_bl);
                    }
                }
            }
            BlockTerminator::FuncCall(_) => { panic!("Blocks pending optimization should not have FuncCall as terminator.") }
            BlockTerminator::ProgTerm() => {}
        }
    }

    // Initialize map from old label of blocks to new labels
    let mut label_map = HashMap::new();
    // Initialize a new list of blocks
    let mut new_bls = Vec::new();

    // Iterate through all blocks. If it was never visited, delete it and update next_bls
    let mut new_label = 0;
    for old_label in 0..old_size {
        label_map.insert(old_label, new_label);
        if visited[old_label] {
            // Change block name to match new_label
            let tmp_bl = Block {
                name: new_label,
                // No need to store statements if we are at the exit block
                instructions: bls[old_label].instructions.clone(),
                terminator: bls[old_label].terminator.clone()
            };
            new_bls.push(tmp_bl);
            new_label += 1;
        }
    }
    let new_entry_bl = *label_map.get(&entry_bl).unwrap();
    let new_size = new_label;

    // Iterate through all new blocks again, update %RP and Block Terminator
    for cur_bl in 0..new_size {

        // If we encounter any %RP = <counter>, update <counter> to label_map[<counter>]
        for i in 0..new_bls[cur_bl].instructions.len() {
            let bc = new_bls[cur_bl].instructions[i].clone();
            if let Some(new_bc) = rp_replacement_stmt(bc, label_map.clone()) {
                let _ = std::mem::replace(&mut new_bls[cur_bl].instructions[i], new_bc);
            }
        }
        
        // Append everything in the terminator of cur_bl to next_bls
        // if they have not been visited before
        let mut new_term: BlockTerminator;
        match new_bls[cur_bl].terminator.clone() {
            BlockTerminator::Transition(t) => {
                let mut new_trans = BlockTransition {
                    cond: t.cond.clone(),
                    tblock: NextBlock::Rp(),
                    fblock: NextBlock::Rp()
                };
                if let NextBlock::Label(tmp_bl) = t.tblock {
                    new_trans.tblock = NextBlock::Label(*label_map.get(&tmp_bl).unwrap());
                }
                if let NextBlock::Label(tmp_bl) = t.fblock {
                    new_trans.fblock = NextBlock::Label(*label_map.get(&tmp_bl).unwrap());
                }
                new_term = BlockTerminator::Transition(new_trans);
            }
            BlockTerminator::Coda(n) => {
                new_term = BlockTerminator::Coda(NextBlock::Rp());
                if let NextBlock::Label(tmp_bl) = n {
                    new_term = BlockTerminator::Coda(NextBlock::Label(*label_map.get(&tmp_bl).unwrap()));
                }
            }
            BlockTerminator::FuncCall(_) => { new_term = new_bls[cur_bl].terminator.clone(); }
            BlockTerminator::ProgTerm() => { new_term = new_bls[cur_bl].terminator.clone(); }
        }
        new_bls[cur_bl].terminator = new_term;
    }
    return (new_bls, new_entry_bl)
}