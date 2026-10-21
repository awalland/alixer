use anyhow::{bail,Result};
use alixer_core::{*,reconcile::Checkpoint};
use rusqlite::{Connection,params};
use serde::{Deserialize,Serialize};
use std::path::Path;
#[derive(Clone,Debug,Default,Serialize,Deserialize)]
pub struct Saved {pub scene:Scene,pub checkpoint:Checkpoint,pub conflicts:Vec<Conflict>}
pub struct Store {db:Connection}
impl Store {
    pub fn open(path:&Path)->Result<Self>{
        let db=Connection::open(path)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS metadata(version INTEGER NOT NULL); INSERT INTO metadata SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM metadata); CREATE TABLE IF NOT EXISTS state(id INTEGER PRIMARY KEY CHECK(id=1), json TEXT NOT NULL); CREATE TABLE IF NOT EXISTS operations(id INTEGER PRIMARY KEY, revision INTEGER NOT NULL, command TEXT NOT NULL, confirmed INTEGER NOT NULL DEFAULT 0);")?;
        let version:i64=db.query_row("SELECT version FROM metadata",[],|r|r.get(0))?;
        if version!=1{bail!("Unsupported database schema {version}; refusing to overwrite");}
        Ok(Self{db})
    }
    pub fn load(&self)->Result<Saved>{
        let mut stmt=self.db.prepare("SELECT json FROM state WHERE id=1")?;
        let mut rows=stmt.query([])?;
        if let Some(row)=rows.next()?{let json:String=row.get(0)?;let saved:Saved=serde_json::from_str(&json)?;saved.scene.validate()?;Ok(saved)}else{Ok(Saved::default())}
    }
    pub fn save(&mut self,saved:&Saved,command:Option<&str>)->Result<()>{
        let tx=self.db.transaction()?;
        tx.execute("INSERT INTO state(id,json) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET json=excluded.json",[serde_json::to_string(saved)?])?;
        if let Some(command)=command{tx.execute("INSERT INTO operations(revision,command) VALUES(?1,?2)",params![i64::try_from(saved.scene.revision)?,command])?;}
        tx.commit()?;Ok(())
    }
    pub fn pending(&self)->Result<i64>{Ok(self.db.query_row("SELECT COUNT(*) FROM operations WHERE confirmed=0",[],|r|r.get(0))?)}
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn committed_intent_survives_without_an_audio_confirmation(){
        let root=std::env::temp_dir().join(format!("alixer-store-{}",Id::new_v4()));std::fs::create_dir_all(&root).unwrap();
        {let mut db=Store::open(&root.join("state.db")).unwrap();let mut saved=Saved::default();saved.scene.edit(Edit::AddBus{name:"Music".into(),aux:false,channels:vec!["FL".into(),"FR".into()]}).unwrap();db.save(&saved,Some("AddBus")).unwrap();}
        let db=Store::open(&root.join("state.db")).unwrap();assert_eq!(db.load().unwrap().scene.strips[0].name,"Music");assert_eq!(db.pending().unwrap(),1);drop(db);std::fs::remove_dir_all(root).unwrap();
    }
}
