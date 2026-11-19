//! `ForkFile` lookups and the recursive statement walker.

use crate::syntax::*;

/// Visit every statement, including the bodies of `on` blocks, in source order.
pub(crate) fn each_stmt<'a>(statements: &'a [Stmt], visit: &mut dyn FnMut(&'a Stmt)) {
    for statement in statements {
        visit(statement);
        if let Stmt::On(block) = statement {
            each_stmt(&block.body, visit);
        }
    }
}
impl ForkFile {
    /// `true` when `id` names any declared entity (project, feature, resource, service, task).
    pub fn has_id(&self, id: &str) -> bool {
        let mut found = false;
        each_stmt(&self.statements, &mut |statement| {
            if found {
                return;
            }
            found = match statement {
                Stmt::Project(value) => value.value == id,
                Stmt::Features { definitions, .. } => definitions
                    .iter()
                    .any(|definition| definition.name.value == id),
                Stmt::Postgres(cluster) => {
                    cluster.id.value == id
                        || cluster
                            .databases
                            .iter()
                            .any(|database| database.id.value == id)
                        || cluster.on.iter().any(|block| {
                            block
                                .databases
                                .iter()
                                .any(|database| database.id.value == id)
                        })
                }
                Stmt::Redis(cluster) => cluster.id.value == id,
                Stmt::S3(bucket) => bucket.id.value == id,
                Stmt::Domain(domain) => domain.id.value == id,
                Stmt::EnvGroup(group) => group.name.value == id,
                Stmt::Service(service) => service.id.value == id,
                Stmt::Task(task) => task.name.value == id,
                Stmt::On(_) => false,
            };
        });
        found
    }

    /// Find a declared service by id.
    pub fn find_service(&self, id: &str) -> Option<&ServiceDef> {
        let mut found = None;
        each_stmt(&self.statements, &mut |statement| {
            if found.is_some() {
                return;
            }
            if let Stmt::Service(service) = statement
                && service.id.value == id
            {
                found = Some(service);
            }
        });
        found
    }

    /// Find a service's container source by service id.
    pub fn find_image(&self, id: &str) -> Option<&ImageSource> {
        self.find_service(id).map(|service| &service.source)
    }

    /// Find a postgres cluster by id.
    pub fn find_pg_cluster(&self, id: &str) -> Option<&PgClusterDef> {
        let mut found = None;
        each_stmt(&self.statements, &mut |statement| {
            if found.is_some() {
                return;
            }
            if let Stmt::Postgres(cluster) = statement
                && cluster.id.value == id
            {
                found = Some(cluster);
            }
        });
        found
    }

    /// Find a postgres database by id, together with its owning cluster.
    pub fn find_pg_database(&self, id: &str) -> Option<(&PgClusterDef, &PgDatabaseDef)> {
        let mut found = None;
        each_stmt(&self.statements, &mut |statement| {
            if found.is_some() {
                return;
            }
            let Stmt::Postgres(cluster) = statement else {
                return;
            };
            if let Some(database) = cluster
                .databases
                .iter()
                .find(|database| database.id.value == id)
            {
                found = Some((cluster, database));
                return;
            }
            for block in &cluster.on {
                if let Some(database) = block
                    .databases
                    .iter()
                    .find(|database| database.id.value == id)
                {
                    found = Some((cluster, database));
                    return;
                }
            }
        });
        found
    }

    /// Find a redis cluster by id.
    pub fn find_redis_cluster(&self, id: &str) -> Option<&RedisClusterDef> {
        let mut found = None;
        each_stmt(&self.statements, &mut |statement| {
            if found.is_some() {
                return;
            }
            if let Stmt::Redis(cluster) = statement
                && cluster.id.value == id
            {
                found = Some(cluster);
            }
        });
        found
    }
}
